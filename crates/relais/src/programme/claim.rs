//! Programme claims: which session holds which ticket, so two sessions that
//! finish at a boundary minutes apart are not handed the same next ticket.
//!
//! [`rules`] holds the pure part: when a claim holds, which tickets are
//! eligible, in what order programmes are entered. [`store`] holds the files
//! and the locks that write them. Selection ([`next`]) takes the programmes
//! as values, already read from git, so it can be tested without a
//! repository.

pub mod rules;
pub mod store;

use std::collections::BTreeSet;

use chrono::NaiveDate;

pub use rules::{
    held, repo_key, resolve_session, Claim, ClaimKey, ClaimRecord, Claimant, Contest, Elsewhere,
    Hold, Holder, Intent, Kind, Next, Pick, ProgrammeState, ReadyTicket, Resolution, SessionClaim,
    SessionClaims, Unreadable, FUTURE_SKEW_SECS, HANDOFF_HOLD_SECS, UNPARSEABLE_HOLD_SECS,
    WORK_HOLD_SECS,
};
pub use store::{
    now_unix, Acquired, ClaimError, Event, Pass, Released, SlugLock, Store, Taken, LOCK_WAIT,
};

use rules::{eligible, order_destinations, own_handoff, unheld_afk, Destination};

/// How [`next`] writes a `handoff` reservation. The CLI hands in the
/// shared state's node half (`shared_state::node::NodeReservations`),
/// which asks the shared half first and then writes the claim file.
pub trait Reservations {
    type Error: From<ClaimError>;

    /// Reserve `id` for `session`'s next session (the rules of
    /// [`SlugLock::reserve_handoff`]); whether it was reserved.
    fn handoff(
        &self,
        lock: &SlugLock<'_>,
        key: &ClaimKey,
        id: &str,
        session: &str,
        repo_display: &str,
        now: i64,
    ) -> Result<bool, Self::Error>;

    /// Reserve the first candidate of an empty programme (the rules of
    /// [`SlugLock::reserve_in_empty_programme`]).
    fn in_empty_programme(
        &self,
        lock: &SlugLock<'_>,
        programme: &EmptyProgramme<'_>,
    ) -> Result<Option<String>, Self::Error>;
}

/// A handoff reservation in a programme no ticket of which is held.
#[derive(Debug, Clone, Copy)]
pub struct EmptyProgramme<'a> {
    pub key: &'a ClaimKey,
    /// The eligible tickets, in order; the first is reserved.
    pub candidates: &'a [&'a str],
    pub open: &'a BTreeSet<String>,
    pub branch_held: &'a BTreeSet<String>,
    pub session: &'a str,
    pub repo_display: &'a str,
    pub now: i64,
}

/// Picks the next ticket for a session at a boundary.
///
/// With a `from` programme (a session handing off from it), the choice never
/// leaves it (ADR-0084):
///
/// 0. With [`Intent::Reserve`], the ticket a live `handoff` claim of the
///    session already holds in `from`, so a re-run names the same ticket.
///    One it holds in another programme is left to expire.
/// 1. The first unheld, eligible ready ticket of `from`.
/// 2. Otherwise [`Next::Parked`], with the date of `from`'s soonest future
///    ticket and, read without a lock, the other programmes in which nothing
///    is held and some ticket is eligible ([`Next::Parked::elsewhere`]).
///
/// Without `from`, no programme is being left: step 0 looks in every
/// programme, then the first eligible ready ticket of a programme in which no
/// ticket is held is chosen, programmes ordered like the root summary
/// (soonest due, then most ready, then name), else [`Next::Parked`].
///
/// When reserving, the choice is written as a `handoff` claim; a lost race
/// moves on to the following candidate. `from`'s lock is taken once and its
/// claims swept once; a `from` lock not acquired within [`LOCK_WAIT`] is
/// [`ClaimError::LockTimeout`]. A destination's lock not acquired in time
/// counts as occupied. Each reservation goes through `reservations` (the
/// CLI's asks the shared state first): a ticket it refuses is passed over
/// like one the claim file holds.
pub fn next<R: Reservations + ?Sized>(
    reservations: &R,
    store: &Store,
    from: Option<&ProgrammeState>,
    others: &[ProgrammeState],
    today: NaiveDate,
    now: i64,
    intent: Intent<'_>,
) -> Result<Next, R::Error> {
    if let Intent::Reserve { session } = intent {
        let scope: Vec<&ProgrammeState> = match from {
            Some(state) => vec![state],
            None => others.iter().collect(),
        };
        for state in scope {
            let records = store.records(&state.key)?;
            if let Some((id, name)) = own_handoff(state, &records, session, now) {
                return Ok(Next::Pick(pick_of(state, id, name)));
            }
        }
    }
    match from {
        Some(state) => next_in(reservations, store, state, others, today, now, intent),
        None => next_anywhere(reservations, store, others, today, now, intent),
    }
}

fn pick_of(state: &ProgrammeState, id: &str, name: &str) -> Pick {
    Pick {
        repo: state.repo.clone(),
        slug: state.slug.clone(),
        id: id.to_string(),
        name: name.to_string(),
    }
}

/// The soonest due date after `today` among `tickets`.
fn soonest_future_due(tickets: &[&ReadyTicket], today: NaiveDate) -> Option<NaiveDate> {
    tickets
        .iter()
        .filter_map(|ticket| ticket.due.filter(|due| *due > today))
        .min()
}

/// Steps 1 and 2 with a `from` programme: its tickets only, else the park.
fn next_in<R: Reservations + ?Sized>(
    reservations: &R,
    store: &Store,
    state: &ProgrammeState,
    others: &[ProgrammeState],
    today: NaiveDate,
    now: i64,
    intent: Intent<'_>,
) -> Result<Next, R::Error> {
    let wake = match intent {
        Intent::Look => {
            let held_ids = store.held_ids(&state.key, now)?;
            let unheld = unheld_afk(state, &held_ids);
            if let Some(ticket) = unheld.iter().find(|t| eligible(t, today)) {
                return Ok(Next::Pick(pick_of(state, &ticket.id, &ticket.name)));
            }
            soonest_future_due(&unheld, today)
        }
        Intent::Reserve { session } => {
            let lock = store.lock(&state.key, LOCK_WAIT, now)?;
            lock.sweep(&state.closed, now)?;
            let held_ids = lock.held_ids(now)?;
            let unheld = unheld_afk(state, &held_ids);
            for ticket in unheld.iter().filter(|t| eligible(t, today)) {
                if reservations.handoff(
                    &lock,
                    &state.key,
                    &ticket.id,
                    session,
                    &state.repo_path,
                    now,
                )? {
                    return Ok(Next::Pick(pick_of(state, &ticket.id, &ticket.name)));
                }
            }
            soonest_future_due(&unheld, today)
        }
    };
    let (elsewhere, unreadable) = ready_elsewhere(store, others, today, now);
    Ok(Next::Parked {
        next_due: wake,
        elsewhere,
        unreadable,
    })
}

/// The other programmes as `next` sees them without a lock.
struct Survey<'a> {
    /// Programmes in which nothing is held and some ticket is eligible, in
    /// the root summary's order.
    destinations: Vec<Destination<'a>>,
    /// The soonest future due among their unheld `afk` tickets, held
    /// programmes included.
    wake: Option<NaiveDate>,
    /// Programmes whose claims could not be read.
    unreadable: Vec<(&'a ProgrammeState, ClaimError)>,
}

fn survey<'a>(
    store: &Store,
    others: &'a [ProgrammeState],
    today: NaiveDate,
    now: i64,
) -> Survey<'a> {
    let mut destinations = Vec::new();
    let mut wake: Option<NaiveDate> = None;
    let mut unreadable = Vec::new();
    for state in others {
        let held_ids: BTreeSet<String> = match store.held_ids(&state.key, now) {
            Ok(ids) => ids
                .into_iter()
                .filter(|id| state.open.contains_key(id))
                .collect(),
            Err(error) => {
                unreadable.push((state, error));
                continue;
            }
        };
        let unheld = unheld_afk(state, &held_ids);
        if let Some(due) = soonest_future_due(&unheld, today) {
            wake = Some(wake.map_or(due, |soonest| soonest.min(due)));
        }
        let is_empty = held_ids.is_empty() && state.branch_held.is_empty();
        let eligible_tickets: Vec<&ReadyTicket> = unheld
            .into_iter()
            .filter(|ticket| eligible(ticket, today))
            .collect();
        if is_empty && !eligible_tickets.is_empty() {
            destinations.push(Destination {
                state,
                eligible: eligible_tickets,
            });
        }
    }
    Survey {
        destinations: order_destinations(destinations),
        wake,
        unreadable,
    }
}

/// The ready-elsewhere list of a park, and the programmes left out of it
/// because their claims could not be read.
fn ready_elsewhere(
    store: &Store,
    others: &[ProgrammeState],
    today: NaiveDate,
    now: i64,
) -> (Vec<Elsewhere>, Vec<Unreadable>) {
    let survey = survey(store, others, today, now);
    let elsewhere = survey
        .destinations
        .iter()
        .map(|destination| Elsewhere {
            repo: destination.state.repo.clone(),
            slug: destination.state.slug.clone(),
            ready: destination.eligible.len(),
        })
        .collect();
    let unreadable = survey
        .unreadable
        .into_iter()
        .map(|(state, error)| Unreadable {
            repo: state.repo.clone(),
            slug: state.slug.clone(),
            cause: error.to_string(),
        })
        .collect();
    (elsewhere, unreadable)
}

/// Without a `from` programme: the first programme in which nothing is
/// held, else the park.
fn next_anywhere<R: Reservations + ?Sized>(
    reservations: &R,
    store: &Store,
    others: &[ProgrammeState],
    today: NaiveDate,
    now: i64,
    intent: Intent<'_>,
) -> Result<Next, R::Error> {
    let survey = survey(store, others, today, now);
    if let Some((_, error)) = survey.unreadable.into_iter().next() {
        return Err(error.into());
    }
    for Destination { state, eligible } in &survey.destinations {
        let first = eligible[0];
        let Intent::Reserve { session } = intent else {
            return Ok(Next::Pick(pick_of(state, &first.id, &first.name)));
        };
        let lock = match store.lock(&state.key, LOCK_WAIT, now) {
            Ok(lock) => lock,
            Err(ClaimError::LockTimeout { .. }) => continue,
            Err(e) => return Err(e.into()),
        };
        lock.sweep(&state.closed, now)?;
        let ids: Vec<&str> = eligible.iter().map(|t| t.id.as_str()).collect();
        let open_ids: BTreeSet<String> = state.open.keys().cloned().collect();
        if let Some(id) = reservations.in_empty_programme(
            &lock,
            &EmptyProgramme {
                key: &state.key,
                candidates: &ids,
                open: &open_ids,
                branch_held: &state.branch_held,
                session,
                repo_display: &state.repo_path,
                now,
            },
        )? {
            let name = eligible
                .iter()
                .find(|t| t.id == id)
                .map_or(first.name.as_str(), |t| t.name.as_str());
            return Ok(Next::Pick(pick_of(state, &id, name)));
        }
    }
    Ok(Next::Parked {
        next_due: survey.wake,
        elsewhere: Vec::new(),
        unreadable: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::programme::model::Mode;
    use crate::test_support::short_temp_dir;
    use std::path::Path;

    const NOW: i64 = 1_800_000_000;

    fn claim(kind: Kind, claimed_ago: i64, refreshed_ago: i64) -> ClaimRecord {
        ClaimRecord::Parsed(Claim {
            session: "s1".into(),
            kind,
            claimed_at: NOW - claimed_ago,
            refreshed_at: NOW - refreshed_ago,
            from: None,
            repo: String::new(),
        })
    }

    fn day(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, day).expect("a date")
    }

    #[test]
    fn a_work_claim_holds_for_n_after_its_refresh_and_not_a_second_longer() {
        assert!(held(&claim(Kind::Work, 99_999, WORK_HOLD_SECS), NOW));
        assert!(!held(&claim(Kind::Work, 0, WORK_HOLD_SECS + 1), NOW));
        // Only the refresh counts for work: an old claim, freshly refreshed, holds.
        assert!(held(&claim(Kind::Work, WORK_HOLD_SECS * 3, 5), NOW));
    }

    #[test]
    fn a_handoff_claim_holds_for_r_after_it_was_made_and_not_a_second_longer() {
        assert!(held(&claim(Kind::Handoff, HANDOFF_HOLD_SECS, 0), NOW));
        assert!(!held(&claim(Kind::Handoff, HANDOFF_HOLD_SECS + 1, 0), NOW));
        // A handoff is not extended by a refresh.
        assert!(!held(&claim(Kind::Handoff, HANDOFF_HOLD_SECS + 1, 1), NOW));
    }

    #[test]
    fn a_claim_that_does_not_parse_holds_for_a_minute() {
        let unparsed = |age: i64| ClaimRecord::Unparsed {
            modified_at: NOW - age,
        };
        assert!(held(&unparsed(UNPARSEABLE_HOLD_SECS - 1), NOW));
        assert!(held(&unparsed(UNPARSEABLE_HOLD_SECS), NOW));
        assert!(!held(&unparsed(UNPARSEABLE_HOLD_SECS + 1), NOW));
    }

    #[test]
    fn the_repo_key_is_twelve_hex_characters_of_the_sha256() {
        // sha256("abc") = ba7816bf8f01cfea414140de5dae2223…
        assert_eq!(repo_key(Path::new("abc")), "ba7816bf8f01");
    }

    struct Fixture {
        scratch: crate::test_support::TempDir,
        store: Store,
    }

    fn fixture(tag: &str) -> Fixture {
        let scratch = short_temp_dir(&format!("claim-{tag}"));
        let store = Store::in_state_dir(&scratch);
        Fixture { scratch, store }
    }

    fn ready(id: &str, due: Option<NaiveDate>, mode: Mode) -> ReadyTicket {
        ReadyTicket {
            id: id.into(),
            name: format!("name of {id}"),
            due,
            mode,
        }
    }

    fn programme(repo: &str, slug: &str, ready: Vec<ReadyTicket>) -> ProgrammeState {
        let open = ready
            .iter()
            .map(|t| (t.id.clone(), t.name.clone()))
            .collect();
        ProgrammeState {
            repo: repo.into(),
            repo_path: format!("/r/{repo}"),
            slug: slug.into(),
            key: ClaimKey {
                repo_key: repo_key(Path::new(repo)),
                slug: slug.into(),
            },
            ready,
            open,
            closed: BTreeSet::new(),
            branch_held: BTreeSet::new(),
        }
    }

    fn picked(next: &Next) -> String {
        match next {
            Next::Pick(p) => format!("{}:{}:{}", p.repo, p.slug, p.id),
            Next::Parked { .. } => "parked".to_string(),
        }
    }

    fn afk(id: &str) -> ReadyTicket {
        ready(id, None, Mode::Afk)
    }

    #[test]
    fn the_from_programme_comes_first_then_a_programme_with_nothing_held() {
        let fixture = fixture("order");
        let from = programme("a", "own", vec![afk("t1"), afk("t2")]);
        let other = programme("b", "far", vec![afk("x1")]);
        let chosen = |from: Option<&ProgrammeState>| {
            picked(
                &next(
                    &crate::shared_state::node::NodeReservations(
                        &crate::shared_state::LocalState::new(),
                    ),
                    &fixture.store,
                    from,
                    std::slice::from_ref(&other),
                    day(19),
                    NOW,
                    Intent::Look,
                )
                .unwrap(),
            )
        };
        assert_eq!(chosen(Some(&from)), "a:own:t1");
        assert_eq!(chosen(None), "b:far:x1");

        // A held first ticket is passed over inside the from programme.
        let lock = fixture.store.lock(&from.key, LOCK_WAIT, NOW).unwrap();
        assert!(lock.reserve_handoff("t1", "s9", "/r/a", NOW).unwrap());
        drop(lock);
        assert_eq!(chosen(Some(&from)), "a:own:t2");
    }

    fn reserve(
        fixture: &Fixture,
        from: Option<&ProgrammeState>,
        others: &[ProgrammeState],
        session: &str,
    ) -> Result<Next, crate::shared_state::node::NodeError> {
        next(
            &crate::shared_state::node::NodeReservations(&crate::shared_state::LocalState::new()),
            &fixture.store,
            from,
            others,
            day(19),
            NOW,
            Intent::Reserve { session },
        )
    }

    #[test]
    fn an_exhausted_from_programme_parks_and_lists_the_empty_ones_without_reserving_there() {
        let fixture = fixture("stay");
        let from = programme("a", "own", vec![ready("later", Some(day(24)), Mode::Afk)]);
        // A sooner date elsewhere is not the park's date: it is not this programme's.
        let far = programme(
            "b",
            "far",
            vec![afk("x1"), ready("x2", Some(day(20)), Mode::Afk)],
        );
        let busy = programme("c", "busy", vec![afk("y1"), afk("y2")]);
        fixture
            .store
            .lock(&busy.key, LOCK_WAIT, NOW)
            .unwrap()
            .reserve_handoff("y1", "s9", "/r/c", NOW)
            .unwrap();
        let many = programme("d", "many", vec![afk("z1"), afk("z2")]);
        let others = [far.clone(), busy, many.clone()];
        let parked = reserve(&fixture, Some(&from), &others, "s1").unwrap();
        let elsewhere = |repo: &str, slug: &str, ready: usize| Elsewhere {
            repo: repo.into(),
            slug: slug.into(),
            ready,
        };
        assert_eq!(
            parked,
            Next::Parked {
                next_due: Some(day(24)),
                // Most ready first; the programme with a held ticket is left out.
                elsewhere: vec![elsewhere("d", "many", 2), elsewhere("b", "far", 1)],
                unreadable: Vec::new(),
            }
        );
        assert!(fixture.store.records(&far.key).unwrap().is_empty());
        assert!(fixture.store.records(&many.key).unwrap().is_empty());
        assert!(fixture.store.records(&from.key).unwrap().is_empty());
        // Without a programme to leave, the same call still enters one.
        assert_eq!(
            picked(&reserve(&fixture, None, &others, "s1").unwrap()),
            "d:many:z1"
        );
    }

    #[test]
    fn an_own_handoff_in_another_programme_is_not_returned_from_a_from_programme() {
        let fixture = fixture("ownelse");
        let from = programme("a", "own", Vec::new());
        let far = programme("b", "far", vec![afk("x1")]);
        fixture
            .store
            .lock(&far.key, LOCK_WAIT, NOW)
            .unwrap()
            .reserve_handoff("x1", "s1", "/r/b", NOW)
            .unwrap();
        let others = [far];
        assert_eq!(
            reserve(&fixture, Some(&from), &others, "s1").unwrap(),
            Next::Parked {
                next_due: None,
                elsewhere: Vec::new(),
                unreadable: Vec::new(),
            }
        );
        assert_eq!(
            picked(&reserve(&fixture, None, &others, "s1").unwrap()),
            "b:far:x1"
        );
    }

    #[test]
    fn a_busy_from_lock_is_a_lock_timeout_not_a_way_into_another_programme() {
        let fixture = fixture("busyfrom");
        let from = programme("a", "own", vec![afk("t1")]);
        let far = programme("b", "far", vec![afk("x1")]);
        let held = fixture.store.lock(&from.key, LOCK_WAIT, NOW).unwrap();
        let outcome = reserve(&fixture, Some(&from), std::slice::from_ref(&far), "s1");
        drop(held);
        assert!(
            matches!(
                outcome,
                Err(crate::shared_state::node::NodeError::Claim(
                    ClaimError::LockTimeout { .. }
                ))
            ),
            "{outcome:?}"
        );
        assert!(fixture.store.records(&far.key).unwrap().is_empty());
    }

    #[test]
    fn claims_that_cannot_be_read_elsewhere_leave_that_programme_out_of_the_park() {
        let fixture = fixture("unread");
        let from = programme("a", "own", Vec::new());
        let broken = programme("b", "broken", vec![afk("x1")]);
        let fine = programme("c", "fine", vec![afk("y1")]);
        // A file where the programme's claim directory belongs: listing it fails.
        let dir = fixture.store.claims_dir_for_test(&broken.key);
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::write(&dir, "not a directory").unwrap();
        let others = [broken, fine];
        let Next::Parked {
            elsewhere,
            unreadable,
            ..
        } = reserve(&fixture, Some(&from), &others, "s1").unwrap()
        else {
            panic!("expected a park");
        };
        assert_eq!(
            elsewhere,
            vec![Elsewhere {
                repo: "c".into(),
                slug: "fine".into(),
                ready: 1
            }]
        );
        assert_eq!(unreadable.len(), 1);
        assert_eq!(
            (unreadable[0].repo.as_str(), unreadable[0].slug.as_str()),
            ("b", "broken"),
            "{unreadable:?}"
        );
        // Without a programme to leave, an unreadable programme still fails.
        assert!(reserve(&fixture, None, &others, "s1").is_err());
    }

    #[test]
    fn a_programme_with_a_held_ticket_is_not_entered_and_the_rest_are_ordered_like_the_summary() {
        let fixture = fixture("dest");
        let busy = programme("a", "busy", vec![afk("t1"), afk("t2")]);
        fixture
            .store
            .lock(&busy.key, LOCK_WAIT, NOW)
            .unwrap()
            .reserve_handoff("t1", "s9", "/r/a", NOW)
            .unwrap();
        let mut branch = programme("b", "branch", vec![afk("t1"), afk("t2")]);
        branch.branch_held.insert("t9".into());
        let few = programme("c", "few", vec![afk("t1")]);
        let many = programme("d", "many", vec![afk("t1"), afk("t2")]);
        let dated = programme("e", "dated", vec![ready("t1", Some(day(10)), Mode::Afk)]);
        let others = [busy, branch, few, many, dated];
        let chosen = |others: &[ProgrammeState]| {
            picked(
                &next(
                    &crate::shared_state::node::NodeReservations(
                        &crate::shared_state::LocalState::new(),
                    ),
                    &fixture.store,
                    None,
                    others,
                    day(19),
                    NOW,
                    Intent::Look,
                )
                .unwrap(),
            )
        };
        assert_eq!(chosen(&others), "e:dated:t1", "soonest due first");
        assert_eq!(chosen(&others[..4]), "d:many:t1", "then most ready");
        assert_eq!(chosen(&others[..3]), "c:few:t1", "busy and branch skipped");
    }

    #[test]
    fn a_future_due_or_a_ticket_for_the_person_is_never_selected_and_the_park_names_the_date() {
        let fixture = fixture("park");
        let from = programme(
            "a",
            "own",
            vec![
                ready("later", Some(day(24)), Mode::Afk),
                ready("soon", Some(day(21)), Mode::Afk),
                ready("person", None, Mode::Hitl),
            ],
        );
        let run = |today| {
            next(
                &crate::shared_state::node::NodeReservations(
                    &crate::shared_state::LocalState::new(),
                ),
                &fixture.store,
                Some(&from),
                &[],
                today,
                NOW,
                Intent::Look,
            )
            .unwrap()
        };
        assert_eq!(
            run(day(19)),
            Next::Parked {
                next_due: Some(day(21)),
                elsewhere: Vec::new(),
                unreadable: Vec::new(),
            }
        );
        assert_eq!(picked(&run(day(21))), "a:own:soon");
        let only_person = programme("b", "p", vec![ready("p", None, Mode::Hitl)]);
        assert_eq!(
            next(
                &crate::shared_state::node::NodeReservations(
                    &crate::shared_state::LocalState::new()
                ),
                &fixture.store,
                None,
                &[only_person],
                day(19),
                NOW,
                Intent::Look
            )
            .unwrap(),
            Next::Parked {
                next_due: None,
                elsewhere: Vec::new(),
                unreadable: Vec::new(),
            }
        );
        assert_eq!(Next::parked_line(None), "parked: nothing due");
        assert_eq!(
            Next::parked_line(Some(day(21))),
            "parked: next due 2026-11-21"
        );
    }

    #[test]
    fn a_reserving_next_makes_a_handoff_claim_so_the_second_caller_takes_the_next_ticket() {
        let fixture = fixture("reserve");
        let from = programme("a", "own", vec![afk("t1"), afk("t2")]);
        let take = |session: &str| {
            picked(
                &next(
                    &crate::shared_state::node::NodeReservations(
                        &crate::shared_state::LocalState::new(),
                    ),
                    &fixture.store,
                    Some(&from),
                    &[],
                    day(19),
                    NOW,
                    Intent::Reserve { session },
                )
                .unwrap(),
            )
        };
        assert_eq!(take("s1"), "a:own:t1");
        assert_eq!(take("s2"), "a:own:t2");
        assert_eq!(take("s3"), "parked");
        let records = fixture.store.records(&from.key).unwrap();
        assert!(
            matches!(&records["t1"], ClaimRecord::Parsed(c) if c.kind == Kind::Handoff && c.session == "s1")
        );
        let log = std::fs::read_to_string(fixture.scratch.to_path_buf().join("claims/log.jsonl"))
            .unwrap();
        assert_eq!(log.lines().count(), 2);
        assert!(log.contains("\"event\":\"create\""));
    }

    #[test]
    fn two_callers_entering_one_empty_programme_reserve_at_most_one_ticket_there() {
        let fixture = fixture("empty");
        let destination = programme("z", "empty", vec![afk("t1"), afk("t2")]);
        let one = next(
            &crate::shared_state::node::NodeReservations(&crate::shared_state::LocalState::new()),
            &fixture.store,
            None,
            std::slice::from_ref(&destination),
            day(19),
            NOW,
            Intent::Reserve { session: "s1" },
        )
        .unwrap();
        let two = next(
            &crate::shared_state::node::NodeReservations(&crate::shared_state::LocalState::new()),
            &fixture.store,
            None,
            std::slice::from_ref(&destination),
            day(19),
            NOW,
            Intent::Reserve { session: "s2" },
        )
        .unwrap();
        assert_eq!(picked(&one), "z:empty:t1");
        assert_eq!(picked(&two), "parked");
    }

    #[test]
    fn claiming_takes_over_a_handoff_refuses_a_live_work_claim_and_steals_on_request() {
        let fixture = fixture("claim");
        let key = ClaimKey {
            repo_key: "k".into(),
            slug: "p".into(),
        };
        let lock = fixture.store.lock(&key, LOCK_WAIT, NOW).unwrap();
        assert!(lock.reserve_handoff("t1", "old", "/r", NOW).unwrap());
        assert_eq!(
            lock.claim_work("t1", "a", "/r", Contest::Refuse, NOW)
                .unwrap(),
            Taken::Claimed {
                how: Acquired::TookOver,
                from: Some(Holder::Session {
                    id: "old".into(),
                    kind: Kind::Handoff
                })
            }
        );
        assert_eq!(
            lock.claim_work("t1", "b", "/r", Contest::Refuse, NOW + 5)
                .unwrap(),
            Taken::Refused {
                holder: Holder::Session {
                    id: "a".into(),
                    kind: Kind::Work
                },
                age_secs: 5
            }
        );
        assert_eq!(
            lock.claim_work("t1", "a", "/r", Contest::Refuse, NOW + 9)
                .unwrap(),
            Taken::Claimed {
                how: Acquired::Refreshed,
                from: None
            }
        );
        assert_eq!(
            lock.claim_work("t1", "b", "/r", Contest::Steal, NOW + 9)
                .unwrap(),
            Taken::Claimed {
                how: Acquired::Stole,
                from: Some(Holder::Session {
                    id: "a".into(),
                    kind: Kind::Work
                })
            }
        );
        assert_eq!(
            lock.release("t1", "a", NOW + 9).unwrap(),
            Released::NotHolder {
                holder: Holder::Session {
                    id: "b".into(),
                    kind: Kind::Work
                }
            }
        );
        assert_eq!(
            lock.release("t1", "b", NOW + 9).unwrap(),
            Released::Released
        );
        assert_eq!(lock.release("t1", "b", NOW + 9).unwrap(), Released::Absent);
    }

    #[test]
    fn an_expired_claim_is_replaced_and_a_closed_tickets_claim_is_swept() {
        let fixture = fixture("sweep");
        let key = ClaimKey {
            repo_key: "k".into(),
            slug: "p".into(),
        };
        let lock = fixture.store.lock(&key, LOCK_WAIT, NOW).unwrap();
        lock.claim_work("t1", "a", "/r", Contest::Refuse, NOW)
            .unwrap();
        lock.claim_work("t2", "a", "/r", Contest::Refuse, NOW)
            .unwrap();
        let closed: BTreeSet<String> = ["t2".to_string()].into();
        let later = NOW + WORK_HOLD_SECS + 1;
        lock.sweep(&closed, NOW).unwrap();
        assert_eq!(fixture.store.records(&key).unwrap().len(), 1);
        assert_eq!(
            lock.claim_work("t1", "b", "/r", Contest::Refuse, later)
                .unwrap(),
            Taken::Claimed {
                how: Acquired::Created,
                from: None
            }
        );
    }

    #[test]
    fn a_session_refresh_moves_only_its_own_work_claims_past_n() {
        let fixture = fixture("refresh");
        let key = ClaimKey {
            repo_key: "k".into(),
            slug: "p".into(),
        };
        {
            let lock = fixture.store.lock(&key, LOCK_WAIT, NOW).unwrap();
            lock.claim_work("t1", "a", "/r", Contest::Refuse, NOW)
                .unwrap();
            lock.claim_work("t2", "b", "/r", Contest::Refuse, NOW)
                .unwrap();
        }
        let later = NOW + WORK_HOLD_SECS - 1;
        assert_eq!(
            fixture.store.refresh_session_once("a", later).unwrap().done,
            1
        );
        let after = later + 2;
        assert_eq!(
            fixture.store.held_ids(&key, after).unwrap(),
            ["t1".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn a_claim_stamped_more_than_a_minute_ahead_of_the_clock_does_not_hold() {
        assert!(held(
            &claim(Kind::Work, -FUTURE_SKEW_SECS, -FUTURE_SKEW_SECS),
            NOW
        ));
        assert!(!held(
            &claim(Kind::Work, -FUTURE_SKEW_SECS - 1, -FUTURE_SKEW_SECS - 1),
            NOW
        ));
    }

    fn key_of(slug: &str) -> ClaimKey {
        ClaimKey {
            repo_key: "k".into(),
            slug: slug.into(),
        }
    }

    #[test]
    fn an_ending_session_skips_a_failing_programme_and_still_leaves_its_tombstone() {
        let fixture = fixture("end-fail");
        {
            let lock = fixture.store.lock(&key_of("good"), LOCK_WAIT, NOW).unwrap();
            lock.claim_work("t1", "s1", "/r", Contest::Refuse, NOW)
                .unwrap();
        }
        // A directory named like a claim file cannot be read as one: this programme errors.
        let bad = fixture.scratch.to_path_buf().join("claims/k/bad/x.toml");
        std::fs::create_dir_all(&bad).unwrap();
        // A wide grace: a concurrent fork may hold a copy of the lock for a moment.
        let pass = fixture
            .store
            .end_session_within("s1", NOW, std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(pass.done, 1, "the good programme is still cleared");
        assert_eq!(pass.skipped, 1);
        let lines = pass.failure_lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("claims k/bad: "), "{lines:?}");
        assert!(fixture.store.records(&key_of("good")).unwrap().is_empty());
        assert!(fixture
            .scratch
            .to_path_buf()
            .join("claims/ended/s1")
            .is_file());
    }

    /// A lock that reads as held for a moment after its holder let go (a
    /// concurrent fork copies the descriptor until it execs) is waited for,
    /// not skipped: the session's claims are cleared and no tombstone is left.
    #[test]
    fn an_ending_session_waits_out_a_lock_held_for_a_moment() {
        let fixture = fixture("end-brief");
        {
            let lock = fixture.store.lock(&key_of("p"), LOCK_WAIT, NOW).unwrap();
            lock.claim_work("t1", "s1", "/r", Contest::Refuse, NOW)
                .unwrap();
        }
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let state_dir = fixture.scratch.to_path_buf();
        let holder = std::thread::spawn(move || {
            let store = Store::in_state_dir(&state_dir);
            let _lock = store.lock(&key_of("p"), LOCK_WAIT, NOW).unwrap();
            held_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
        });
        held_rx.recv().unwrap();
        // A wide grace keeps this about waiting, not about timing; the hook
        // tests check that a busy lock costs the production grace, not LOCK_WAIT.
        let pass = fixture
            .store
            .end_session_within("s1", NOW, std::time::Duration::from_secs(2))
            .unwrap();
        holder.join().unwrap();
        assert_eq!((pass.done, pass.skipped), (1, 0));
        assert!(fixture.store.records(&key_of("p")).unwrap().is_empty());
        assert!(!fixture
            .scratch
            .to_path_buf()
            .join("claims/ended/s1")
            .exists());
    }

    #[test]
    fn a_tombstone_that_cannot_be_applied_does_not_keep_the_lock_from_the_caller() {
        let fixture = fixture("tombstone-fail");
        // `ended` as a file: listing the tombstones fails.
        let claims = fixture.scratch.to_path_buf().join("claims");
        std::fs::create_dir_all(&claims).unwrap();
        std::fs::write(claims.join("ended"), "not a directory").unwrap();
        let lock = fixture.store.lock(&key_of("p"), LOCK_WAIT, NOW).unwrap();
        assert_eq!(
            lock.claim_work("t1", "a", "/r", Contest::Refuse, NOW)
                .unwrap(),
            Taken::Claimed {
                how: Acquired::Created,
                from: None
            }
        );
    }

    #[test]
    fn an_unreadable_claim_is_a_holder_of_its_own_kind() {
        let fixture = fixture("unreadable");
        let dir = fixture.scratch.to_path_buf().join("claims/k/p");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t1.toml"), "not = [toml").unwrap();
        let claim_now = now_unix();
        let lock = fixture
            .store
            .lock(&key_of("p"), LOCK_WAIT, claim_now)
            .unwrap();
        assert_eq!(
            lock.claim_work("t1", "a", "/r", Contest::Refuse, claim_now)
                .unwrap(),
            Taken::Refused {
                holder: Holder::Unreadable,
                age_secs: 0
            }
        );
        assert_eq!(
            lock.release("t1", "a", claim_now).unwrap(),
            Released::NotHolder {
                holder: Holder::Unreadable
            }
        );
    }

    fn claimed_ticket(resolution: &Resolution) -> Option<(&str, Claimant)> {
        match resolution {
            Resolution::Claimed { claim, claimant } => Some((claim.id.as_str(), *claimant)),
            Resolution::Nothing { .. } => None,
        }
    }

    fn write_work(store: &Store, programme: &str, id: &str, session: &str, now: i64) {
        let lock = store.lock(&key_of(programme), LOCK_WAIT, now).unwrap();
        lock.claim_work(id, session, "/r", Contest::Refuse, now)
            .unwrap();
    }

    fn write_handoff(store: &Store, programme: &str, id: &str, session: &str, now: i64) {
        let lock = store.lock(&key_of(programme), LOCK_WAIT, now).unwrap();
        assert!(lock.reserve_handoff(id, session, "/r", now).unwrap());
    }

    #[test]
    fn own_work_beats_own_handoff() {
        let fixture = fixture("resolve-work");
        write_handoff(&fixture.store, "p", "t1", "s1", NOW);
        write_work(&fixture.store, "p", "t2", "s1", NOW);
        let own = fixture.store.claims_of("s1", NOW).unwrap();
        let resolution = resolve_session(&own, None);
        assert_eq!(claimed_ticket(&resolution), Some(("t2", Claimant::Own)));
    }

    #[test]
    fn own_handoff_beats_a_previous_handoff() {
        let fixture = fixture("resolve-handoff");
        write_handoff(&fixture.store, "p", "t1", "s1", NOW);
        write_handoff(&fixture.store, "q", "t9", "s0", NOW);
        let own = fixture.store.claims_of("s1", NOW).unwrap();
        let previous = fixture.store.claims_of("s0", NOW).unwrap();
        let resolution = resolve_session(&own, Some(&previous));
        assert_eq!(claimed_ticket(&resolution), Some(("t1", Claimant::Own)));
        let only_previous = resolve_session(&SessionClaims::default(), Some(&previous));
        assert_eq!(
            claimed_ticket(&only_previous),
            Some(("t9", Claimant::Previous))
        );
    }

    #[test]
    fn a_previous_work_claim_is_never_chosen() {
        let fixture = fixture("resolve-previous-work");
        write_work(&fixture.store, "p", "t1", "s0", NOW);
        let previous = fixture.store.claims_of("s0", NOW).unwrap();
        let resolution = resolve_session(&SessionClaims::default(), Some(&previous));
        assert_eq!(resolution, Resolution::Nothing { expired: false });
    }

    #[test]
    fn an_expired_claim_gives_nothing_with_the_expired_reason() {
        let fixture = fixture("resolve-expired");
        write_work(&fixture.store, "p", "t1", "s1", NOW - WORK_HOLD_SECS - 10);
        let own = fixture.store.claims_of("s1", NOW).unwrap();
        assert!(own.live.is_empty());
        assert_eq!(
            resolve_session(&own, None),
            Resolution::Nothing { expired: true }
        );
    }

    #[test]
    fn a_work_claim_of_an_ended_session_is_ignored() {
        let fixture = fixture("resolve-ended");
        write_work(&fixture.store, "p", "t1", "s1", NOW);
        write_handoff(&fixture.store, "p", "t2", "s1", NOW);
        fixture.store.mark_ended("s1", NOW).unwrap();
        let own = fixture.store.claims_of("s1", NOW).unwrap();
        let kinds: Vec<Kind> = own.live.iter().map(|found| found.claim.kind).collect();
        assert_eq!(kinds, vec![Kind::Handoff]);
        assert!(own.expired.is_empty());
    }
}
