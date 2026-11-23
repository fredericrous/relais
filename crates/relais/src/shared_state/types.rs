//! The plain public types the [`super::SharedState`] trait speaks: who
//! holds a thing, which thing, the fence that proves the hold is current,
//! and the spend and publish records an external primary keeps.

use crate::money::MicroUsd;

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

/// What a publish came to: rows taken now, rows already known (a replay),
/// rows queued for later, and the highest sequence the sink holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PublishReceipt {
    pub accepted: u64,
    pub duplicates: u64,
    pub queued: u64,
    pub through_seq: Option<u64>,
}
