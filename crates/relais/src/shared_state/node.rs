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
//! [`SharedStateError::Compensation`] with the release failure beside it.
//! An unreleased shared hold ends by its TTL (tickets) or a `replacing`
//! claim (intents).
//!
//! This module is public because the CLI is a separate binary target. It
//! is relais's own: a closed client implements [`SharedState`], never these.

use super::types::{DecisionGrant, IntentAnswer, Pointer, Swap, TicketAnswer, TicketRequest};
use super::{SharedState, SharedStateError, TicketClaim, TicketGrant};
use crate::ids::RunId;
use crate::learn::registry::{EvaluatedArtifact, Registry};
use crate::ledger::{
    Claim, DecisionAnswer, IntentState, Ledger, NewIntent, SuccessorIntent, SuccessorProcess,
};
use crate::money::MicroUsd;
use crate::procs::LockFile;
use crate::programme::claim::{Holder, Kind, Store, Taken, LOCK_WAIT};

/// How long a registry swap waits for another swap's lock.
const REGISTRY_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// `local` as the caller sees it, with a failed release joined to it.
fn compensated(local: SharedStateError, release: Result<(), SharedStateError>) -> SharedStateError {
    match release {
        Ok(()) => local,
        Err(release) => SharedStateError::Compensation {
            local: Box::new(local),
            release: Box::new(release),
        },
    }
}

/// A release after a local refusal: the refusal stays the answer, so a
/// failed release is said on stderr, never dropped.
fn give_back(what: &str, release: Result<(), SharedStateError>) {
    if let Err(e) = release {
        eprintln!("relais: the shared hold on {what} could not be given back: {e}");
    }
}

/// What has been spent since `day_start`: the global counter when the
/// implementation keeps one, else this node's ledger.
pub fn daily_spent(
    state: &dyn SharedState,
    ledger: &Ledger,
    day_start: &str,
) -> Result<MicroUsd, SharedStateError> {
    match state.daily_spent(day_start)? {
        Some(spent) => Ok(spent),
        None => Ok(ledger.spend_since(day_start)?),
    }
}

/// The claim file's rules: sweep the programme's closed tickets, then
/// claim `claim.id` as `work`.
fn claim_in_store(store: &Store, claim: &TicketClaim<'_>) -> Result<Taken, SharedStateError> {
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
) -> Result<TicketGrant, SharedStateError> {
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

/// Claim a successor intent for the run minted for it.
pub fn claim_intent(
    state: &dyn SharedState,
    ledger: &Ledger,
    intent_id: &str,
    run: &RunId,
    alive: &dyn Fn(u32) -> SuccessorProcess,
) -> Result<Claim, SharedStateError> {
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
) -> Result<Claim, SharedStateError> {
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
    let give_back = |why: &str| ledger.release_claim(intent_id, run, why);
    match state.claim_intent(intent_id, run, Some(old)) {
        Ok(IntentAnswer::Granted) => Ok(Claim::Claimed(claimed)),
        Ok(IntentAnswer::HeldBy { run: holder }) => {
            // The row is left pending; the next claim is refused by the
            // shared half again while `holder` holds it.
            give_back(&format!("the shared claim is held by run {holder}"))?;
            Ok(Claim::AlreadyClaimed { run: holder })
        }
        Err(shared) => {
            let released = give_back(&format!("the shared claim failed: {shared}"))
                .map(|_| ())
                .map_err(SharedStateError::from);
            Err(compensated(shared, released))
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
) -> Result<bool, SharedStateError> {
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
) -> Result<Option<SuccessorIntent>, SharedStateError> {
    match state.answer_decision(run)? {
        DecisionGrant::AlreadyAnswered => Ok(None),
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
) -> Result<Option<String>, SharedStateError> {
    match state.registry_pointer()? {
        Pointer::Shared(pointer) => Ok(pointer),
        Pointer::NodeDecides => Ok(registry.active_id()?),
    }
}

/// Point the registry at `evaluated` when the active pointer still names
/// `expected`; else [`SharedStateError::CasConflict`].
pub fn swap_registry_pointer(
    state: &dyn SharedState,
    registry: &Registry,
    expected: Option<&str>,
    evaluated: &EvaluatedArtifact,
) -> Result<(), SharedStateError> {
    let to = evaluated.artifact_id();
    match state.swap_registry_pointer(expected, Some(to))? {
        Swap::NodeDecides => swap_locally(registry, expected, evaluated),
        // The shared compare-and-set decided; the local promote follows.
        // A promote killed between the two leaves the shared pointer ahead
        // until the next promote or rollback.
        Swap::Swapped => match registry.promote(evaluated) {
            Ok(()) => Ok(()),
            Err(local) => {
                let back = state.swap_registry_pointer(Some(to), expected).map(|_| ());
                Err(compensated(local.into(), back))
            }
        },
    }
}

/// Compare and promote the node's own pointer files, under an flock.
fn swap_locally(
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
