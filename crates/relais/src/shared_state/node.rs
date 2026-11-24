//! The node half of each shared operation: the local write on the handles
//! the caller holds (the ledger, the claim store, the registry), composed
//! with the shared half the [`SharedState`] implementation answers in plain
//! values.
//!
//! Every function runs in one order: the shared half first, then the local
//! write, then one release when the local step refuses after a shared
//! grant. Under [`super::LocalState`] the shared half grants or says the
//! node decides, so each function runs exactly the code the trait method
//! ran before it lost its handles.
//!
//! A failed release never replaces the local answer: a refusal is returned
//! as it is (the release failure is one line on stderr), and a local error
//! comes back as
//! [`NodeError::Compensation`] with the release failure beside it.
//! An unreleased shared hold ends by its TTL (tickets) or a `replacing`
//! claim (intents).
//!
//! This module is public because the CLI is a separate binary target. It
//! is relais's own: a closed client implements [`SharedState`], never these.

use std::path::PathBuf;

use super::types::{
    DecisionGrant, IntentAnswer, LeaseToken, Pointer, Principal, Swap, TicketAnswer, TicketRequest,
};
use super::{SharedState, SharedStateError};
use crate::ids::RunId;
use crate::learn::cycle::{PointerRefusal, PointerWrites};
use crate::learn::registry::{ArtifactError, EvaluatedArtifact, Registry};
use crate::ledger::{
    Claim, DecisionAnswer, IntentState, Ledger, NewIntent, SuccessorIntent, SuccessorProcess,
};
use crate::money::MicroUsd;
use crate::procs::LockFile;
use crate::programme::claim::{
    ClaimError, ClaimKey, Contest, EmptyProgramme, Holder, Kind, Pass, Reservations, SlugLock,
    Store, Taken, LOCK_WAIT,
};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// Why a node function did not do what it was asked: the shared
/// half's own refusal, or a failure of the node's own files and ledger.
///
/// This is relais's type, not the closed client's: a client implements
/// [`SharedState`] and returns [`SharedStateError`] only. The ledger,
/// claim and registry errors below are what the node half adds around it.
#[derive(Debug)]
pub enum NodeError {
    /// The shared half answered with an error. Transparent: it displays and
    /// chains as the wrapped error does.
    Shared(SharedStateError),
    /// A local lock could not be taken (not held by another: it failed).
    Lock {
        path: PathBuf,
        cause: std::io::Error,
    },
    /// The node's own ledger.
    Ledger(crate::ledger::LedgerError),
    /// The programme claim store.
    Claim(crate::programme::claim::ClaimError),
    /// The learned-artifact registry.
    Registry(crate::learn::registry::ArtifactError),
    /// The local step failed after a shared grant, and giving the shared
    /// hold back failed too. `local` is what the caller would have seen
    /// alone; `release` is why the shared hold is still there.
    Compensation {
        local: Box<NodeError>,
        release: Box<NodeError>,
    },
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::Shared(e) => write!(f, "{e}"),
            NodeError::Lock { path, cause } => {
                write!(f, "cannot lock {}: {cause}", path.display())
            }
            NodeError::Ledger(e) => write!(f, "{e}"),
            NodeError::Claim(e) => write!(f, "{e}"),
            NodeError::Registry(e) => write!(f, "{e}"),
            NodeError::Compensation { local, release } => write!(
                f,
                "{local} (and the shared hold could not be given back: {release})"
            ),
        }
    }
}

impl std::error::Error for NodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NodeError::Shared(e) => e.source(),
            NodeError::Lock { cause, .. } => Some(cause),
            NodeError::Ledger(e) => Some(e),
            NodeError::Claim(e) => Some(e),
            NodeError::Registry(e) => Some(e),
            NodeError::Compensation { local, .. } => Some(local.as_ref()),
        }
    }
}

impl From<SharedStateError> for NodeError {
    fn from(e: SharedStateError) -> Self {
        NodeError::Shared(e)
    }
}

impl From<crate::ledger::LedgerError> for NodeError {
    fn from(e: crate::ledger::LedgerError) -> Self {
        NodeError::Ledger(e)
    }
}

impl From<crate::programme::claim::ClaimError> for NodeError {
    fn from(e: crate::programme::claim::ClaimError) -> Self {
        NodeError::Claim(e)
    }
}

impl From<crate::learn::registry::ArtifactError> for NodeError {
    fn from(e: crate::learn::registry::ArtifactError) -> Self {
        NodeError::Registry(e)
    }
}

/// One request to take a programme ticket as `work`: the input of
/// [`claim_ticket`], which turns it into a [`TicketRequest`] for the
/// shared half and keeps the claim file's own fields (`closed`,
/// `repo_display`, `now`) on the node.
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

/// A ticket claim's answer ([`claim_ticket`]), and the fence of the
/// hold when it was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketGrant {
    pub taken: Taken,
    /// `Some` when [`Taken::Claimed`]; `None` when refused.
    pub lease: Option<LeaseToken>,
}

/// How long a registry swap waits for another swap's lock.
const REGISTRY_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// `local` as the caller sees it, with a failed release joined to it.
fn compensated<E: Into<NodeError>>(local: NodeError, release: Result<(), E>) -> NodeError {
    match release {
        Ok(()) => local,
        Err(release) => NodeError::Compensation {
            local: Box::new(local),
            release: Box::new(release.into()),
        },
    }
}

/// A release after a local refusal: the refusal stays the answer, so a
/// failed release is said on stderr, never dropped.
fn give_back<E: std::fmt::Display>(what: &str, release: Result<(), E>) {
    if let Err(e) = release {
        eprintln!("relais: the shared hold on {what} could not be given back: {e}");
    }
}

// holds-until: every spend reaches the global counter: the runner's own
// settles, a dispatch whose reserve failed after launch, the settles a
// restarted coordinator lost and the settles the primary refused (dropped by
// the seat book; follow-ups to an-t17). Then the global counter alone is the
// day.
/// What has been spent since `day_start`: the greater of the global counter
/// (when the implementation keeps one) and this node's ledger. The
/// coordinator's settles feed the counter (an-t17), but some spend still
/// reaches only the ledger, and local spend is a lower bound of global
/// spend, so the cap never counts less than the ledger says.
pub fn daily_spent(
    state: &dyn SharedState,
    ledger: &Ledger,
    day_start: &str,
) -> Result<MicroUsd, NodeError> {
    let local = ledger.spend_since(day_start)?;
    Ok(match state.daily_spent(day_start)? {
        Some(global) => global.max(local),
        None => local,
    })
}

/// The claim file's rules: sweep the programme's closed tickets, then
/// claim `claim.id` as `work`.
fn claim_in_store(store: &Store, claim: &TicketClaim<'_>) -> Result<Taken, NodeError> {
    let lock = store.lock(claim.key, LOCK_WAIT, claim.now)?;
    lock.sweep(claim.closed, claim.now)?;
    Ok(lock.claim_work(
        claim.id,
        &claim.holder.id,
        claim.repo_display,
        claim.contest,
        claim.now,
    )?)
}

/// Take a programme ticket as `work` (the rules of
/// [`crate::programme::claim::SlugLock::claim_work`]).
pub fn claim_ticket(
    state: &dyn SharedState,
    store: &Store,
    claim: &TicketClaim<'_>,
) -> Result<TicketGrant, NodeError> {
    let request = TicketRequest {
        repo_key: claim.key.repo_key.clone(),
        slug: claim.key.slug.clone(),
        id: claim.id.to_string(),
        holder: claim.holder.clone(),
        contest: claim.contest,
    };
    match state.claim_ticket(&request)? {
        TicketAnswer::NodeDecides => {
            // The claim file decides first; only a claim it wrote is fenced,
            // so a refused attempt never moves the holder's epoch.
            let taken = claim_in_store(store, claim)?;
            let lease = match &taken {
                Taken::Claimed { .. } => Some(state.grant_ticket(&request)?),
                Taken::Refused { .. } => None,
            };
            Ok(TicketGrant { taken, lease })
        }
        TicketAnswer::Held { holder, age_secs } => Ok(TicketGrant {
            taken: Taken::Refused {
                holder: Holder::Session {
                    id: holder.id,
                    kind: Kind::Work,
                },
                age_secs,
            },
            lease: None,
        }),
        TicketAnswer::Granted(lease) => match claim_in_store(store, claim) {
            Ok(taken @ Taken::Claimed { .. }) => Ok(TicketGrant {
                taken,
                lease: Some(lease),
            }),
            Ok(refused) => {
                // The refusal is the answer; a hold that cannot be given
                // back ends by its TTL.
                give_back(&lease.resource.to_string(), state.release_ticket(&lease));
                Ok(TicketGrant {
                    taken: refused,
                    lease: None,
                })
            }
            Err(local) => Err(compensated(local, state.release_ticket(&lease))),
        },
    }
}

/// The request the shared half sees for `id` in `key`'s programme.
fn ticket_request(key: &ClaimKey, id: &str, holder: &Principal) -> TicketRequest {
    TicketRequest {
        repo_key: key.repo_key.clone(),
        slug: key.slug.clone(),
        id: id.to_string(),
        holder: holder.clone(),
        contest: Contest::Refuse,
    }
}

/// Reserve `id` as a `handoff` claim for `holder`'s next session (the
/// rules of [`SlugLock::reserve_handoff`]), the shared half first; whether
/// it was reserved. A ticket the shared half says is held is not reserved,
/// as a ticket the claim file holds is not. A crash between the shared
/// grant and the claim file leaves a shared hold that ends at
/// [`crate::programme::claim::HANDOFF_HOLD_SECS`].
pub fn reserve_handoff(
    state: &dyn SharedState,
    lock: &SlugLock<'_>,
    key: &ClaimKey,
    id: &str,
    holder: &Principal,
    repo_display: &str,
    now: i64,
) -> Result<bool, NodeError> {
    let request = ticket_request(key, id, holder);
    match state.reserve_handoff(&request)? {
        TicketAnswer::NodeDecides => Ok(lock.reserve_handoff(id, &holder.id, repo_display, now)?),
        TicketAnswer::Held { .. } => Ok(false),
        TicketAnswer::Granted(lease) => {
            match lock.reserve_handoff(id, &holder.id, repo_display, now) {
                Ok(true) => Ok(true),
                Ok(false) => {
                    // holds-until: a caller asks the shared half for a ticket
                    // whose claim file is already written. Today every caller
                    // asks only for tickets the claim files do not hold, under
                    // this `SlugLock`; since a primary re-grants its own
                    // holder's retry (an-t23), such a call would give back a
                    // shared hold the claim file still keeps.
                    give_back(&lease.resource.to_string(), state.release_ticket(&lease));
                    Ok(false)
                }
                Err(local) => Err(compensated(local.into(), state.release_ticket(&lease))),
            }
        }
    }
}

/// Reserve the first of the candidates as a `handoff` claim, only when the
/// programme is empty (the rules of [`SlugLock::reserve_in_empty_programme`]),
/// the shared half first for that ticket.
pub fn reserve_in_empty_programme(
    state: &dyn SharedState,
    lock: &SlugLock<'_>,
    programme: &EmptyProgramme<'_>,
    holder: &Principal,
) -> Result<Option<String>, NodeError> {
    let Some(first) = programme.candidates.first() else {
        return Ok(None);
    };
    let local = |lock: &SlugLock<'_>| {
        lock.reserve_in_empty_programme(
            programme.candidates,
            programme.open,
            programme.branch_held,
            &holder.id,
            programme.repo_display,
            programme.now,
        )
    };
    match state.reserve_handoff(&ticket_request(programme.key, first, holder))? {
        TicketAnswer::NodeDecides => Ok(local(lock)?),
        TicketAnswer::Held { .. } => Ok(None),
        TicketAnswer::Granted(lease) => match local(lock) {
            Ok(Some(reserved)) => Ok(Some(reserved)),
            Ok(None) => {
                give_back(&lease.resource.to_string(), state.release_ticket(&lease));
                Ok(None)
            }
            Err(local) => Err(compensated(local.into(), state.release_ticket(&lease))),
        },
    }
}

/// `relais programme next --claim`'s reservations through the shared
/// state: the CLI hands one to [`crate::programme::claim::next`]. The
/// session reserving is the holder, as `relais programme claim` names it.
pub struct NodeReservations<'a>(pub &'a dyn SharedState);

impl Reservations for NodeReservations<'_> {
    type Error = NodeError;

    fn handoff(
        &self,
        lock: &SlugLock<'_>,
        key: &ClaimKey,
        id: &str,
        session: &str,
        repo_display: &str,
        now: i64,
    ) -> Result<bool, NodeError> {
        let holder = Principal::operator(session);
        reserve_handoff(self.0, lock, key, id, &holder, repo_display, now)
    }

    fn in_empty_programme(
        &self,
        lock: &SlugLock<'_>,
        programme: &EmptyProgramme<'_>,
    ) -> Result<Option<String>, NodeError> {
        let holder = Principal::operator(programme.session);
        reserve_in_empty_programme(self.0, lock, programme, &holder)
    }
}

/// What a pass over a session's claims did: the shared half's answer and
/// the claim files' pass. Both always run, so neither hides the other.
#[derive(Debug)]
pub struct SessionPass {
    pub shared: Result<(), SharedStateError>,
    pub local: Result<Pass, ClaimError>,
}

/// A session-wide shared call, bounded by `deadline` when one is given:
/// the call runs on a thread of its own that owns an [`Arc`] of the state
/// and prints nothing, and a caller past its deadline leaves it behind
/// (the hold it was renewing or giving back ends by its TTL).
fn session_call(
    state: &Arc<dyn SharedState>,
    holder: &Principal,
    deadline: Option<Duration>,
    call: fn(&dyn SharedState, &Principal) -> Result<(), SharedStateError>,
) -> Result<(), SharedStateError> {
    let Some(deadline) = deadline else {
        return call(&**state, holder);
    };
    let holder = holder.clone();
    within(state, deadline, move |state| call(state, &holder)).unwrap_or_else(|unanswered| {
        Err(SharedStateError::Unreachable {
            detail: unanswered.to_string(),
        })
    })
}

/// Why [`within`] has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unanswered {
    /// The call was still running at the deadline; it is left behind.
    Deadline(Duration),
    /// The call's thread ended without an answer (it panicked).
    Died,
    /// No thread could be started for it.
    NoThread,
}

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unanswered::Deadline(deadline) => write!(
                f,
                "the shared state did not answer within {} ms",
                deadline.as_millis()
            ),
            Unanswered::Died => write!(f, "the call to the shared state died"),
            Unanswered::NoThread => write!(f, "no thread could be started for the shared state"),
        }
    }
}

/// Run `call` on the state on a thread of its own, which owns an [`Arc`]
/// of it and prints nothing, and wait for its answer at most `deadline`.
/// A caller past its deadline leaves the call behind: it ends with the
/// process, and a hold it was renewing or giving back ends by its TTL.
pub fn within<T: Send + 'static>(
    state: &Arc<dyn SharedState>,
    deadline: Duration,
    call: impl FnOnce(&dyn SharedState) -> T + Send + 'static,
) -> Result<T, Unanswered> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let owned = Arc::clone(state);
    std::thread::Builder::new()
        .name("relais-shared-state".into())
        .spawn(move || {
            let _ = sender.send(call(&*owned)); // after its deadline nobody waits for it
        })
        .map_err(|_| Unanswered::NoThread)?;
    receiver.recv_timeout(deadline).map_err(|e| match e {
        std::sync::mpsc::RecvTimeoutError::Timeout => Unanswered::Deadline(deadline),
        std::sync::mpsc::RecvTimeoutError::Disconnected => Unanswered::Died,
    })
}

/// A session is still working: renew its shared `work` holds, then move
/// its claim files' refresh time to `now` ([`Store::refresh_session_once`]).
pub fn refresh_session(
    state: &Arc<dyn SharedState>,
    store: &Store,
    session: &str,
    now: i64,
    deadline: Option<Duration>,
) -> SessionPass {
    let holder = Principal::operator(session);
    SessionPass {
        shared: session_call(state, &holder, deadline, |s, h| s.renew_tickets(h)),
        local: store.refresh_session_once(session, now),
    }
}

/// A session ended: give back its shared `work` holds, then delete its
/// `work` claim files and keep its `handoff` ones
/// ([`Store::end_session_once`]).
pub fn end_session(
    state: &Arc<dyn SharedState>,
    store: &Store,
    session: &str,
    now: i64,
    deadline: Option<Duration>,
) -> SessionPass {
    let holder = Principal::operator(session);
    SessionPass {
        shared: session_call(state, &holder, deadline, |s, h| s.release_tickets(h)),
        local: store.end_session_once(session, now),
    }
}

/// Claim a successor intent for the run minted for it.
pub fn claim_intent(
    state: &dyn SharedState,
    ledger: &Ledger,
    intent_id: &str,
    run: &RunId,
    alive: &dyn Fn(u32) -> SuccessorProcess,
) -> Result<Claim, NodeError> {
    match state.claim_intent(intent_id, run, None)? {
        IntentAnswer::Granted => match ledger.claim_intent(intent_id, run, alive) {
            Ok(claimed @ Claim::Claimed(_)) => Ok(claimed),
            Ok(refused) => {
                give_back(
                    &format!("intent:{intent_id}"),
                    state.release_intent(intent_id, run),
                );
                Ok(refused)
            }
            Err(local) => Err(compensated(
                local.into(),
                state.release_intent(intent_id, run),
            )),
        },
        IntentAnswer::HeldBy { run: old } => {
            take_over_intent(state, ledger, intent_id, run, &old, alive)
        }
    }
}

/// The shared half names `old`. Only a claim this ledger holds for `old` is
/// judged here (the pid probe and grace, as a single node judges it): a run
/// on another node is never judged by this node's probe.
fn take_over_intent(
    state: &dyn SharedState,
    ledger: &Ledger,
    intent_id: &str,
    run: &RunId,
    old: &RunId,
    alive: &dyn Fn(u32) -> SuccessorProcess,
) -> Result<Claim, NodeError> {
    let held_here = ledger.successor_intent(intent_id)?.is_some_and(|intent| {
        intent.state == IntentState::Claimed && intent.run_id.as_ref() == Some(old)
    });
    if !held_here {
        return Ok(Claim::AlreadyClaimed { run: old.clone() });
    }
    let claimed = match ledger.claim_intent(intent_id, run, alive)? {
        Claim::Claimed(intent) => intent,
        other => return Ok(other),
    };
    let release_local = |why: &str| {
        ledger
            .release_claim(intent_id, run, why)
            .map(|_| ())
            .map_err(NodeError::from)
    };
    match state.claim_intent(intent_id, run, Some(old)) {
        Ok(IntentAnswer::Granted) => Ok(Claim::Claimed(claimed)),
        Ok(IntentAnswer::HeldBy { run: holder }) => {
            // The row is left pending; the next claim is refused by the
            // shared half again while `holder` holds it. The refusal stays
            // the answer even when the local claim cannot be given back.
            give_back(
                &format!("intent:{intent_id} (local claim of run {run})"),
                release_local(&format!("the shared claim is held by run {holder}")),
            );
            Ok(Claim::AlreadyClaimed { run: holder })
        }
        Err(shared) => {
            let released = release_local(&format!("the shared claim failed: {shared}"));
            Err(compensated(shared.into(), released))
        }
    }
}

/// Give an intent claim back when its run never started. The local release
/// runs even when the shared one failed; that failure is then the error.
pub fn release_claim(
    state: &dyn SharedState,
    ledger: &Ledger,
    intent_id: &str,
    run: &RunId,
    refusal: &str,
) -> Result<bool, NodeError> {
    let shared = state.release_intent(intent_id, run);
    let released = match ledger.release_claim(intent_id, run, refusal) {
        Ok(released) => released,
        Err(local) => return Err(compensated(local.into(), shared)),
    };
    shared?;
    Ok(released)
}

/// Answer a decision and write its successor intent, as one operation:
/// the answer and the intent never exist one without the other.
pub fn resolve_decision_with_intent(
    state: &dyn SharedState,
    ledger: &Ledger,
    run: &RunId,
    answer: &DecisionAnswer<'_>,
    intent: &NewIntent,
) -> Result<Option<SuccessorIntent>, NodeError> {
    match state.answer_decision(run)? {
        // Another answerer holds the decision: said as such, never as "no
        // open decision", which this node's ledger may contradict.
        DecisionGrant::HeldElsewhere => Err(SharedStateError::Refused {
            reason: format!("the decision of run {run} is being answered elsewhere"),
        }
        .into()),
        DecisionGrant::Granted => match ledger.resolve_decision_with_intent(run, answer, intent) {
            Ok(Some(written)) => Ok(Some(written)),
            Ok(None) => {
                give_back(&format!("decision:{run}"), state.release_decision(run));
                Ok(None)
            }
            Err(local) => Err(compensated(local.into(), state.release_decision(run))),
        },
    }
}

/// The id the active pointer names.
pub fn registry_pointer(
    state: &dyn SharedState,
    registry: &Registry,
) -> Result<Option<String>, NodeError> {
    match state.registry_pointer()? {
        Pointer::Shared(pointer) => Ok(pointer),
        Pointer::NodeDecides => Ok(registry.active_id()?),
    }
}

/// Point the registry at `evaluated` when the active pointer still names
/// `expected`; else [`SharedStateError::CasConflict`].
///
/// Under the node's pointer lock, taken before any pointer is read and
/// held across the shared compare-and-set and the local write: two
/// writers on one node never interleave, and a shared swap's local half
/// always lands on the pointers it was judged against.
pub fn swap_registry_pointer(
    state: &dyn SharedState,
    registry: &Registry,
    expected: Option<&str>,
    evaluated: &EvaluatedArtifact,
) -> Result<(), NodeError> {
    swap_within(state, registry, expected, evaluated, REGISTRY_LOCK_WAIT)
}

/// [`swap_registry_pointer`] with the lock wait given.
pub(crate) fn swap_within(
    state: &dyn SharedState,
    registry: &Registry,
    expected: Option<&str>,
    evaluated: &EvaluatedArtifact,
    wait: Duration,
) -> Result<(), NodeError> {
    let _lock = pointer_lock(registry, wait)?;
    heal(state, registry)?;
    let to = evaluated.artifact_id();
    match state.swap_registry_pointer(expected, Some(to))? {
        Swap::NodeDecides => {
            compare_locally(registry, expected)?;
            Ok(registry.promote(evaluated)?)
        }
        // The shared compare-and-set decided; the local promote follows.
        Swap::Swapped => match registry.promote(evaluated) {
            Ok(()) => Ok(()),
            Err(local) => {
                let back = state.swap_registry_pointer(Some(to), expected).map(|_| ());
                Err(compensated(local.into(), back))
            }
        },
    }
}

/// Make the registry's previous artifact active again when the active
/// pointer still names `expected` (the artifact the caller judged): the
/// id restored, `None` when there is no previous artifact to return to
/// (none, or it is `expected` itself), else
/// [`SharedStateError::CasConflict`]. Under the same lock as
/// [`swap_registry_pointer`], so a rollback and a promote judged against
/// the same artifact cannot both land.
pub fn roll_back_registry(
    state: &dyn SharedState,
    registry: &Registry,
    expected: &str,
) -> Result<Option<String>, NodeError> {
    roll_back_within(state, registry, expected, REGISTRY_LOCK_WAIT)
}

/// [`roll_back_registry`] with the lock wait given.
pub(crate) fn roll_back_within(
    state: &dyn SharedState,
    registry: &Registry,
    expected: &str,
    wait: Duration,
) -> Result<Option<String>, NodeError> {
    let _lock = pointer_lock(registry, wait)?;
    heal(state, registry)?;
    compare_locally(registry, Some(expected))?;
    let Some(previous) = registry.previous_id()?.filter(|id| id != expected) else {
        return Ok(None);
    };
    match state.swap_registry_pointer(Some(expected), Some(&previous))? {
        Swap::NodeDecides => Ok(registry.rollback()?),
        Swap::Swapped => match registry.rollback() {
            Ok(restored) => Ok(restored),
            Err(local) => {
                let back = state
                    .swap_registry_pointer(Some(&previous), Some(expected))
                    .map(|_| ());
                Err(compensated(local.into(), back))
            }
        },
    }
}

/// The learning cycle's and `relais rollback`'s pointer writes, through
/// [`swap_registry_pointer`] and [`roll_back_registry`]: the CLI builds one
/// around the state it selected and hands it to `learn::cycle`.
pub struct NodePointers(pub Arc<dyn SharedState>);

impl NodePointers {
    fn refusal(e: NodeError) -> PointerRefusal {
        match e {
            NodeError::Shared(SharedStateError::CasConflict { found, .. }) => {
                PointerRefusal::Superseded { found }
            }
            NodeError::Shared(SharedStateError::Refused { reason }) => {
                PointerRefusal::Refused(reason)
            }
            NodeError::Compensation { local, release } => PointerRefusal::Compensated {
                local: local.to_string(),
                release: release.to_string(),
            },
            e @ (NodeError::Shared(_)
            | NodeError::Lock { .. }
            | NodeError::Ledger(_)
            | NodeError::Claim(_)
            | NodeError::Registry(_)) => PointerRefusal::Failed(Box::new(e)),
        }
    }
}

impl PointerWrites for NodePointers {
    fn promote(
        &self,
        registry: &Registry,
        expected: Option<&str>,
        evaluated: &EvaluatedArtifact,
    ) -> Result<(), PointerRefusal> {
        swap_registry_pointer(&*self.0, registry, expected, evaluated).map_err(Self::refusal)
    }

    fn roll_back(
        &self,
        registry: &Registry,
        expected: &str,
    ) -> Result<Option<String>, PointerRefusal> {
        roll_back_registry(&*self.0, registry, expected).map_err(Self::refusal)
    }
}

/// The node's own active pointer must name `expected`.
fn compare_locally(registry: &Registry, expected: Option<&str>) -> Result<(), NodeError> {
    let found = registry.active_id()?;
    if found.as_deref() != expected {
        return Err(SharedStateError::CasConflict {
            expected: expected.map(str::to_string),
            found,
        }
        .into());
    }
    Ok(())
}

/// Complete a promote that died between its shared swap and its local
/// one: when the shared pointer names an artifact the node's own active
/// pointer does not, and this node holds that artifact with its evidence,
/// it becomes active here too (one stderr note says so). An artifact this
/// node does not hold, or whose evidence does not check, is a
/// [`SharedStateError::CasConflict`] naming it: the node never guesses.
/// "The node decides" never heals, so one node behaves as it always did.
/// The caller holds the pointer lock.
fn heal(state: &dyn SharedState, registry: &Registry) -> Result<(), NodeError> {
    let Pointer::Shared(Some(shared)) = state.registry_pointer()? else {
        return Ok(());
    };
    let local = registry.active_id()?;
    if local.as_deref() == Some(shared.as_str()) {
        return Ok(());
    }
    let evaluated = match registry.evaluated(&shared) {
        Ok(evaluated) => evaluated,
        // The node cannot read its own files: that is the error, not the
        // shared pointer.
        Err(e @ ArtifactError::Io(_)) => return Err(e.into()),
        Err(
            why @ (ArtifactError::Malformed(_)
            | ArtifactError::IncompatibleSchema { .. }
            | ArtifactError::DimensionMismatch { .. }
            | ArtifactError::NonFinite
            | ArtifactError::Unknown { .. }
            | ArtifactError::NoEvidence { .. }
            | ArtifactError::EvidenceMismatch { .. }
            | ArtifactError::GatesFailed { .. }
            | ArtifactError::VerdictNotReproducible { .. }),
        ) => {
            return Err(SharedStateError::Refused {
                reason: format!(
                    "the shared registry pointer names {shared}, which this node cannot make \
                     active ({why}); nothing changed, and nothing run here moves past it until \
                     {shared} is stored here with its evidence or another node moves the shared \
                     pointer"
                ),
            }
            .into())
        }
    };
    registry.promote(&evaluated)?;
    eprintln!(
        "relais: the shared registry pointer names {shared} and this node's named {}; \
         {shared} is now active here too (a promote stopped between its two halves)",
        local.as_deref().unwrap_or("nothing")
    );
    Ok(())
}

/// The node's pointer lock: an flock beside the pointer files, outside
/// `artifacts/` (which `list` reads), released by the kernel when the
/// process dies and never deleted, so a killed writer leaves nothing
/// stale. The wait is bounded ([`REGISTRY_LOCK_WAIT`]); a lock still held
/// after it is [`SharedStateError::Refused`].
fn pointer_lock(registry: &Registry, wait: Duration) -> Result<LockFile, NodeError> {
    let artifacts = registry.artifacts_dir();
    let path = artifacts
        .parent()
        .unwrap_or(artifacts)
        .join(".pointer.lock");
    let started = std::time::Instant::now();
    loop {
        match LockFile::try_acquire(&path) {
            Ok(Some(lock)) => return Ok(lock),
            Ok(None) if started.elapsed() < wait => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(None) => {
                return Err(SharedStateError::Refused {
                    reason: format!(
                        "another relais process holds the registry lock {} \
                         (waited {}s); nothing changed, run it again",
                        path.display(),
                        wait.as_secs()
                    ),
                }
                .into())
            }
            Err(cause) => return Err(NodeError::Lock { path, cause }),
        }
    }
}

#[cfg(test)]
mod daily_spent_tests {
    use super::*;
    use crate::shared_state::fake::FakeState;

    #[test]
    fn the_larger_of_the_global_counter_and_the_ledger_is_the_day() {
        let dir = crate::test_support::temp_dir("daily-spent-max");
        let ledger = Ledger::open(&dir.join("ledger.db")).expect("ledger");
        let state = FakeState::new();
        let day = "2026-01-01T00:00:00+00:00";
        assert_eq!(
            daily_spent(&state, &ledger, day).expect("read"),
            MicroUsd::from_micros(0)
        );
        state.set_spent(MicroUsd::from_micros(7));
        assert_eq!(
            daily_spent(&state, &ledger, day).expect("read"),
            MicroUsd::from_micros(7),
            "global above an empty ledger"
        );
        // The ledger above a lower global count wins: the cap never counts
        // less than this node's own spend.
        ledger
            .insert_run(
                &RunId::from_stored("earlier-today"),
                "/elsewhere",
                None,
                &crate::ids::TaskId::from_stored("task-earlier-today"),
                "rk",
            )
            .expect("run");
        ledger
            .record_usage(&crate::ledger::UsageEvent {
                event_id: "today".into(),
                run_id: RunId::from_stored("earlier-today"),
                attempt_id: None,
                parent_event_id: None,
                model: Some("sonnet".into()),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost: Some(MicroUsd::from_micros(50)),
                cost_kind: crate::money::CostKind::ApiSpend,
                completeness: crate::money::CostCompleteness::Actual,
                inclusive: false,
                at: "2026-01-01T08:00:00+00:00".into(),
                phase: None,
                duration_ms: None,
                requested_model: None,
                requested_effort: None,
                harness: None,
            })
            .expect("usage");
        assert_eq!(
            daily_spent(&state, &ledger, day).expect("read"),
            MicroUsd::from_micros(50),
            "the ledger above the global count"
        );
        std::fs::remove_dir_all(&dir).ok(); // best-effort temp cleanup
    }
}
