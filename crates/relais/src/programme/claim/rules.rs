//! The pure rules of programme claims: what a claim is, when it holds its
//! ticket, which tickets are eligible and in what order programmes are
//! entered. No filesystem, no git: claims, clocks and programmes arrive as
//! values.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::programme::model::Mode;

/// How long a `work` claim holds a ticket after its last refresh (N): the
/// p99 gap between commits in one session over 14 days was 7.51 h
/// (session-boundaries t26, Phase 0, 2026-11-19).
pub const WORK_HOLD_SECS: i64 = 8 * 3600;
/// How long a `handoff` claim holds a ticket after it was made (R): the
/// relay delivers within seconds, and a person may clear by hand later.
pub const HANDOFF_HOLD_SECS: i64 = 2 * 3600;
/// How long a claim file that does not parse holds its ticket.
pub const UNPARSEABLE_HOLD_SECS: i64 = 60;
/// How far ahead of the clock a claim's timestamp may lie and still count:
/// a clock that skewed is trusted, a claim stamped further out is not.
pub const FUTURE_SKEW_SECS: i64 = 60;

/// What a claim means for its holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The holder is working the ticket.
    Work,
    /// A session that is about to end named the ticket for its successor.
    Handoff,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Handoff => "handoff",
        }
    }
}

/// One claim file's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub session: String,
    pub kind: Kind,
    /// Unix seconds.
    pub claimed_at: i64,
    /// Unix seconds.
    pub refreshed_at: i64,
    /// The session that handed the ticket off, when this claim took one over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The map repository's path, for display.
    #[serde(default)]
    pub repo: String,
}

/// A claim file as found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimRecord {
    Parsed(Claim),
    /// A file that does not parse, with its modification time (Unix seconds).
    Unparsed {
        modified_at: i64,
    },
}

impl ClaimRecord {
    /// Who holds the ticket this record claims.
    pub fn holder(&self) -> Holder {
        match self {
            Self::Parsed(claim) => Holder::Session {
                id: claim.session.clone(),
                kind: claim.kind,
            },
            Self::Unparsed { .. } => Holder::Unreadable,
        }
    }

    /// Seconds since the claim was last made or refreshed.
    pub fn age_secs(&self, now: i64) -> u64 {
        u64::try_from(now.saturating_sub(self.since())).unwrap_or(0)
    }

    /// Unix seconds of the claim's last refresh, or of its file's last write
    /// when it does not parse.
    fn since(&self) -> i64 {
        match self {
            Self::Parsed(claim) => claim.refreshed_at,
            Self::Unparsed { modified_at } => *modified_at,
        }
    }

    pub(super) fn hold(&self) -> Hold {
        Hold {
            holder: self.holder(),
            since: self.since(),
        }
    }
}

/// Whether a claim still holds its ticket at `now`: a `work` claim until
/// N after its last refresh, a `handoff` claim until R after it was made,
/// a file that does not parse for a minute. The limit itself still holds,
/// and a timestamp more than [`FUTURE_SKEW_SECS`] ahead of `now` does not.
pub fn held(record: &ClaimRecord, now: i64) -> bool {
    let (since, limit) = match record {
        ClaimRecord::Parsed(Claim {
            kind: Kind::Work,
            refreshed_at,
            ..
        }) => (*refreshed_at, WORK_HOLD_SECS),
        ClaimRecord::Parsed(Claim {
            kind: Kind::Handoff,
            claimed_at,
            ..
        }) => (*claimed_at, HANDOFF_HOLD_SECS),
        ClaimRecord::Unparsed { modified_at } => (*modified_at, UNPARSEABLE_HOLD_SECS),
    };
    let age = now.saturating_sub(since);
    (-FUTURE_SKEW_SECS..=limit).contains(&age)
}

/// Who holds a ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    Session {
        id: String,
        kind: Kind,
    },
    /// The claim file does not parse.
    Unreadable,
}

impl Holder {
    /// The first 8 characters of the session id; `unreadable` for a claim
    /// file that does not parse.
    pub fn short(&self) -> String {
        match self {
            Self::Session { id, .. } => id.chars().take(8).collect(),
            Self::Unreadable => "unreadable".to_string(),
        }
    }
}

/// A claim that holds its ticket, as the programme views see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub holder: Holder,
    /// Unix seconds of the claim's last refresh, or of its file's last write
    /// when it does not parse.
    pub since: i64,
}

/// The first 12 hex characters of the SHA-256 of a repository's git common
/// directory: the key that names the repository under `claims/`.
pub fn repo_key(common_dir: &std::path::Path) -> String {
    let digest = Sha256::digest(common_dir.to_string_lossy().as_bytes());
    digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Which programme a claim belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimKey {
    pub repo_key: String,
    pub slug: String,
}

/// Whether `record` is a `work` claim of `session`.
pub(super) fn is_work_of(record: &ClaimRecord, session: &str) -> bool {
    matches!(record, ClaimRecord::Parsed(c) if c.session == session && c.kind == Kind::Work)
}

/// One claim of a session, with the programme and ticket it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionClaim {
    pub key: ClaimKey,
    pub id: String,
    pub claim: Claim,
}

/// The claims one session has written: those that still hold at the time
/// of the read, and the kinds of those that no longer do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionClaims {
    pub live: Vec<SessionClaim>,
    pub expired: Vec<Kind>,
}

/// Whose claim answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claimant {
    /// The asking session's own claim.
    Own,
    /// A handoff the session it replaced left behind.
    Previous,
}

/// Which claim a session holds, and why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Claimed {
        claim: SessionClaim,
        claimant: Claimant,
    },
    Nothing {
        /// A claim of the session (or a handoff of its predecessor) was
        /// found, but it no longer holds.
        expired: bool,
    },
}

/// The claim a session holds: its own `work` claim, else its own `handoff`
/// (each the most recently refreshed first), else a live `handoff` of the
/// session it replaced. The replaced session's `work` claims are never
/// read: they end with it, and one a tombstone has not yet removed is not
/// the successor's.
pub fn resolve_session(own: &SessionClaims, previous: Option<&SessionClaims>) -> Resolution {
    let newest = |claims: &SessionClaims, kind: Kind| {
        claims
            .live
            .iter()
            .filter(|found| found.claim.kind == kind)
            .max_by(|a, b| {
                a.claim
                    .refreshed_at
                    .cmp(&b.claim.refreshed_at)
                    .then_with(|| {
                        (&b.key.repo_key, &b.key.slug, &b.id).cmp(&(
                            &a.key.repo_key,
                            &a.key.slug,
                            &a.id,
                        ))
                    })
            })
            .cloned()
    };
    let own_claim = newest(own, Kind::Work).or_else(|| newest(own, Kind::Handoff));
    if let Some(claim) = own_claim {
        return Resolution::Claimed {
            claim,
            claimant: Claimant::Own,
        };
    }
    if let Some(claim) = previous.and_then(|claims| newest(claims, Kind::Handoff)) {
        return Resolution::Claimed {
            claim,
            claimant: Claimant::Previous,
        };
    }
    Resolution::Nothing {
        expired: !own.expired.is_empty()
            || previous.is_some_and(|claims| claims.expired.contains(&Kind::Handoff)),
    }
}

/// What to do when another live session already holds the ticket as `work`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contest {
    /// Leave it with its holder.
    Refuse,
    /// Take it over.
    Steal,
}

impl Contest {
    pub fn from_steal_flag(steal: bool) -> Self {
        if steal {
            Self::Steal
        } else {
            Self::Refuse
        }
    }
}

/// Whether `next` only looks or writes its choice down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent<'a> {
    Look,
    /// The choice is written as a `handoff` claim for this session.
    Reserve {
        session: &'a str,
    },
}

/// A ready ticket, as selection sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyTicket {
    pub id: String,
    pub name: String,
    pub due: Option<NaiveDate>,
    pub mode: Mode,
}

/// One programme as selection sees it: read from git by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgrammeState {
    /// The map repository's directory name.
    pub repo: String,
    /// The map repository's path, recorded in a claim for display.
    pub repo_path: String,
    pub slug: String,
    pub key: ClaimKey,
    /// Open tickets with every blocker closed and no branch, in frontier order.
    pub ready: Vec<ReadyTicket>,
    /// Every open ticket, by id, with its name.
    pub open: BTreeMap<String, String>,
    pub closed: BTreeSet<String>,
    /// Tickets a `prog/<slug>/<id>` branch works.
    pub branch_held: BTreeSet<String>,
}

/// The ticket `next` chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub repo: String,
    pub slug: String,
    pub id: String,
    pub name: String,
}

/// What `next` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Pick(Pick),
    /// Nothing is eligible; the soonest day an otherwise-ready, unheld,
    /// `afk` ticket falls due, if there is one (with a `from` programme,
    /// of that programme only).
    Parked {
        next_due: Option<NaiveDate>,
        /// With a `from` programme: the other programmes in which nothing
        /// is held and some ticket is eligible, for the person to choose
        /// from; `next` enters none of them (ADR-0084).
        elsewhere: Vec<Elsewhere>,
        /// Programmes left out of `elsewhere` because their claims could
        /// not be read.
        unreadable: Vec<Unreadable>,
    },
}

/// A programme a park leaves out of its ready-elsewhere list because its
/// claims could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    pub repo: String,
    pub slug: String,
    /// What reading its claims failed with.
    pub cause: String,
}

/// A programme a park lists as ready elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elsewhere {
    pub repo: String,
    pub slug: String,
    /// Its eligible tickets.
    pub ready: usize,
}

impl Next {
    /// The line `next` prints when it parks.
    pub fn parked_line(next_due: Option<NaiveDate>) -> String {
        match next_due {
            Some(date) => format!("parked: next due {date}"),
            None => "parked: nothing due".to_string(),
        }
    }
}

/// Whether the person is not needed and the ticket is not scheduled later.
pub(super) fn eligible(ticket: &ReadyTicket, today: NaiveDate) -> bool {
    ticket.mode == Mode::Afk && ticket.due.is_none_or(|due| due <= today)
}

/// A programme's unheld ready tickets, `afk` only, in frontier order.
pub(super) fn unheld_afk<'a>(
    state: &'a ProgrammeState,
    held_ids: &BTreeSet<String>,
) -> Vec<&'a ReadyTicket> {
    state
        .ready
        .iter()
        .filter(|ticket| ticket.mode == Mode::Afk && !held_ids.contains(&ticket.id))
        .collect()
}

/// A programme `next` may enter: nothing in it is held, and some ticket is
/// eligible.
pub(super) struct Destination<'a> {
    pub state: &'a ProgrammeState,
    pub eligible: Vec<&'a ReadyTicket>,
}

/// The destinations ordered like the root summary: soonest due, then most
/// ready, then name.
pub(super) fn order_destinations(mut destinations: Vec<Destination<'_>>) -> Vec<Destination<'_>> {
    destinations.sort_by_key(|destination| {
        let soonest = destination.eligible.iter().filter_map(|t| t.due).min();
        (
            soonest.is_none(),
            soonest,
            std::cmp::Reverse(destination.eligible.len()),
            destination.state.repo.clone(),
            destination.state.slug.clone(),
        )
    });
    destinations
}

/// The ticket a live `handoff` claim of `session` already holds in a
/// programme whose ticket is still open and not worked by a branch.
pub(super) fn own_handoff<'a>(
    state: &'a ProgrammeState,
    records: &BTreeMap<String, ClaimRecord>,
    session: &str,
    now: i64,
) -> Option<(&'a str, &'a str)> {
    records.iter().find_map(|(id, record)| {
        let ours = matches!(
            record,
            ClaimRecord::Parsed(Claim { session: holder, kind: Kind::Handoff, .. }) if holder == session
        );
        let (open_id, name) = state.open.get_key_value(id)?;
        (ours && held(record, now) && !state.branch_held.contains(id))
            .then_some((open_id.as_str(), name.as_str()))
    })
}
