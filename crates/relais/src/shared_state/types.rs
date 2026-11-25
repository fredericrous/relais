//! The plain public types the [`super::SharedState`] trait speaks: who
//! holds a thing, which thing, the fence that proves the hold is current,
//! the shared half's answers, and the spend and publish records an
//! external primary keeps. None of them is a handle or borrows one.

use crate::ids::RunId;
use crate::money::MicroUsd;
use crate::policy::{AutoPromote, AutoPromoteOff, RoutingEnvelope, RoutingOff, TrustGrant};
use crate::programme::claim::Contest;

/// What kind of caller a [`Principal`] is (an-t12): the person's own
/// machine, a long-lived node with its own certificate, or one task's pod.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrincipalKind {
    Operator,
    Node,
    Task,
}

/// Who holds a lease or a claim (an-t11's `holder`, an-t12's principal).
/// Distinct from [`crate::programme::claim::Holder`], which is how a claim
/// file reads back.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Principal {
    pub kind: PrincipalKind,
    pub id: String,
}

impl Principal {
    pub fn operator(id: impl Into<String>) -> Self {
        Self {
            kind: PrincipalKind::Operator,
            id: id.into(),
        }
    }

    pub fn node(id: impl Into<String>) -> Self {
        Self {
            kind: PrincipalKind::Node,
            id: id.into(),
        }
    }

    pub fn task(id: impl Into<String>) -> Self {
        Self {
            kind: PrincipalKind::Task,
            id: id.into(),
        }
    }
}

/// One held thing, named the way the primary keys its row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceId {
    /// A coordinator seat for one dispatch on one node.
    Seat { node: String, dispatch: String },
    /// A programme ticket: repository key, programme slug, ticket id.
    Ticket {
        repo_key: String,
        slug: String,
        id: String,
    },
    /// A successor intent claimed by the run minted for it.
    Intent(String),
}

impl std::fmt::Display for ResourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResourceId::Seat { node, dispatch } => write!(f, "seat:{node}/{dispatch}"),
            ResourceId::Ticket { repo_key, slug, id } => {
                write!(f, "ticket:{repo_key}/{slug}/{id}")
            }
            ResourceId::Intent(id) => write!(f, "intent:{id}"),
        }
    }
}

/// The fence of one hold (an-t11): every acquire bumps `epoch`, and a
/// holder write carries the token so a stale one is refused with
/// [`super::SharedStateError::LeaseLost`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseToken {
    pub resource: ResourceId,
    pub holder: Principal,
    pub epoch: u64,
    /// Unix seconds by the primary's clock; `None` where nothing expires
    /// (a single node's own seats).
    pub expires_at: Option<i64>,
}

/// Money set aside for one dispatch under its seat (an-t10, an-t11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub id: String,
    pub seat: LeaseToken,
    pub amount: MicroUsd,
}

/// How a settle was booked. A settle is never refused: a stale seat makes
/// it a true-up of the expired booking (an-t11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    Booked,
    TrueUp,
}

/// The part of the global daily cap this node may spend without asking
/// (an-t10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slice {
    /// No shared cap: a single node's own `[spending]` ceiling, checked
    /// where it always was, is the only bound.
    Unbounded,
    /// A grant for the current UTC day.
    Granted {
        remaining: MicroUsd,
        /// Unix seconds, 00:00 UTC of the next day.
        valid_until: i64,
    },
}

/// Whether the node is talking to its primary (an-t10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Online,
    /// The primary is unreachable and the node runs on its slice and cache:
    /// one run at a time, claims fail closed, publish rows queue.
    Degraded {
        reason: String,
    },
}

/// One append-only row a node publishes, keyed for idempotent replay by
/// node and per-node sequence (an-t01, an-t10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishRow {
    pub node: String,
    pub seq: u64,
    pub table: String,
    pub payload: String,
}

/// One request for a programme ticket, as the primary keys it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketRequest {
    pub repo_key: String,
    pub slug: String,
    pub id: String,
    pub holder: Principal,
    pub contest: Contest,
}

impl TicketRequest {
    /// The resource the request names.
    pub fn resource(&self) -> ResourceId {
        ResourceId::Ticket {
            repo_key: self.repo_key.clone(),
            slug: self.slug.clone(),
            id: self.id.clone(),
        }
    }
}

/// The shared half's answer to a ticket request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketAnswer {
    /// No shared half: the node's claim file decides, and
    /// [`super::SharedState::grant_ticket`] fences a claim it wrote.
    NodeDecides,
    /// The primary granted the hold; the node writes its claim file next.
    Granted(LeaseToken),
    /// Another holder has it.
    Held { holder: Principal, age_secs: u64 },
}

/// The shared half's answer to an intent claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentAnswer {
    /// This run may claim it; the node's ledger writes the claim next.
    Granted,
    /// Another run holds it.
    HeldBy { run: RunId },
}

/// The shared half's answer to answering a decision (keyed by its run).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionGrant {
    Granted,
    /// Another answerer holds the decision: said as such, never as "no
    /// open decision".
    HeldElsewhere,
}

/// How long an implementation that holds a decision grant keeps it when
/// the answer never lands (the holder died before releasing). After this
/// many seconds the next [`DecisionGrant`] request is `Granted` again.
/// `LocalState` holds no grant, so it never lapses one.
pub const DECISION_GRANT_TTL_SECS: i64 = 10 * 60;

/// The learned registry's active pointer, as the shared half knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pointer {
    /// No shared pointer: the node's pointer files are the truth.
    NodeDecides,
    Shared(Option<String>),
}

/// What a shared pointer swap came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swap {
    /// No shared pointer: the node compares and swaps its own files.
    NodeDecides,
    /// The shared pointer moved; the node promotes locally next.
    Swapped,
}

/// What a publish came to: rows taken now, rows already known (a replay),
/// rows queued for later, and the highest sequence the sink holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PublishReceipt {
    pub accepted: u64,
    pub duplicates: u64,
    pub queued: u64,
    pub through_seq: Option<u64>,
}

/// One write of the machine settings, as plain values (an-t20). Each is one
/// of the writes the settings file took before the trait had a settings
/// write; a client applies it to the shared settings, `LocalState` to the
/// node's file under the lock `relais trust grant` always took.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsEdit {
    /// `[trust."<key>"]`: one exact policy (`relais trust grant --key`).
    GrantTrust { key: String, record: TrustGrant },
    /// `[trust_repos."<repo_key>"]`: the repository, once (`relais trust
    /// grant`).
    GrantRepository {
        repo_key: String,
        record: TrustGrant,
    },
    /// `[learning] auto_promote`; `when: IfAbsent` leaves a grant or an `off`
    /// tombstone already there alone (the installer).
    SetAutoPromote {
        grant: AutoPromote,
        when: SettingsWhen,
    },
    /// `[learning]`: the grant removed, the `off` tombstone recorded.
    AutoPromoteOff(AutoPromoteOff),
    /// `[session_routing] envelope`; `when` as above.
    SetRoutingEnvelope {
        envelope: RoutingEnvelope,
        when: SettingsWhen,
    },
    /// `[session_routing]`: the envelope removed, the `off` tombstone
    /// recorded.
    RoutingOff(RoutingOff),
}

/// Whether a [`SettingsEdit`] that records a grant replaces what is there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsWhen {
    /// Record it, replacing a grant or an `off` tombstone (the person's own command).
    Always,
    /// Record it only when there is neither (the installer).
    IfAbsent,
}

/// What a [`SettingsEdit`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SettingsWrite {
    /// The settings changed. `false` when the key was already granted, or
    /// an `IfAbsent` write found a grant or a tombstone.
    pub written: bool,
    /// An `off` edit removed a grant or an envelope that was there.
    pub removed: bool,
}
