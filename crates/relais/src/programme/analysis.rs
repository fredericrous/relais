//! Pure judgement over parsed tickets: where each stands, and what is
//! wrong between them. No filesystem, no git: the branches that work
//! tickets and the repositories that exist arrive as values.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::claim::rules::Hold;
use super::model::{MapDoc, Mode, Problem, Status, Ticket, MAP_FILE};

/// Where a ticket stands against the others and the branches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Open, every blocker closed, no branch works it.
    Frontier,
    /// Open, and these tickets (not closed, or not there) block it.
    Blocked(Vec<String>),
    /// Open, and a branch or a live claim holds it.
    InFlight {
        holder: Holder,
    },
    Closed,
    OutOfScope,
}

/// What holds an in-flight ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// A `prog/<slug>/<id>` branch works it; its tip was committed at that
    /// Unix time.
    Branch {
        branch: String,
        last_commit_unix: i64,
    },
    /// A live claim file holds it.
    Claim(Hold),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub open: usize,
    pub frontier: usize,
    pub blocked: usize,
    pub in_flight: usize,
    pub closed: usize,
    pub out_of_scope: usize,
}

pub fn branch_name(slug: &str, id: &str) -> String {
    format!("prog/{slug}/{id}")
}

/// Seconds since a branch tip was committed, never negative: a committer
/// date in the future reads as 0.
pub fn age_secs(now_unix: i64, last_commit_unix: i64) -> u64 {
    u64::try_from(now_unix.saturating_sub(last_commit_unix)).unwrap_or(0)
}

/// Every ticket's standing. `working` maps the ids that a
/// `prog/<slug>/<id>` branch works to the latest committer date (Unix
/// seconds) among that branch's local and `origin` refs; `claims` maps the
/// ids that a live claim holds. Both arrive as values, read by the caller.
/// A branch wins over a claim, and either wins over blockers, because it is
/// the stronger fact about the ticket.
pub fn standings(
    tickets: &BTreeMap<String, Ticket>,
    slug: &str,
    working: &BTreeMap<String, i64>,
    claims: &BTreeMap<String, Hold>,
) -> BTreeMap<String, Standing> {
    tickets
        .values()
        .map(|ticket| {
            let standing = match ticket.status {
                Status::Closed => Standing::Closed,
                Status::OutOfScope => Standing::OutOfScope,
                Status::Open => match (working.get(&ticket.id), claims.get(&ticket.id)) {
                    (Some(last_commit_unix), _) => Standing::InFlight {
                        holder: Holder::Branch {
                            branch: branch_name(slug, &ticket.id),
                            last_commit_unix: *last_commit_unix,
                        },
                    },
                    (None, Some(hold)) => Standing::InFlight {
                        holder: Holder::Claim(hold.clone()),
                    },
                    (None, None) => {
                        let blockers: Vec<String> = ticket
                            .blocked_by
                            .iter()
                            .filter(|id| tickets.get(*id).map(|t| t.status) != Some(Status::Closed))
                            .cloned()
                            .collect();
                        if blockers.is_empty() {
                            Standing::Frontier
                        } else {
                            Standing::Blocked(blockers)
                        }
                    }
                },
            };
            (ticket.id.clone(), standing)
        })
        .collect()
}

pub fn counts(standings: &BTreeMap<String, Standing>) -> Counts {
    let mut counts = Counts::default();
    for standing in standings.values() {
        match standing {
            Standing::Frontier => counts.frontier += 1,
            Standing::Blocked(_) => counts.blocked += 1,
            Standing::InFlight { .. } => counts.in_flight += 1,
            Standing::Closed => counts.closed += 1,
            Standing::OutOfScope => counts.out_of_scope += 1,
        }
    }
    counts.open = counts.frontier + counts.blocked + counts.in_flight;
    counts
}

/// How one ticket relates to the others.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Relations {
    /// The tickets that still block it, read from a `Blocked` standing;
    /// empty for a ticket of any other standing, even one with an open
    /// `blocked_by` (an in-flight ticket is worked regardless).
    pub open_blockers: Vec<String>,
    /// The tickets that list it in `blocked_by`.
    pub dependents: Vec<String>,
}

/// The relations of every ticket, keyed by id.
pub fn relations(
    tickets: &BTreeMap<String, Ticket>,
    standings: &BTreeMap<String, Standing>,
) -> BTreeMap<String, Relations> {
    tickets
        .keys()
        .map(|id| {
            let open_blockers = match standings.get(id) {
                Some(Standing::Blocked(blockers)) => blockers.clone(),
                Some(
                    Standing::Frontier
                    | Standing::InFlight { .. }
                    | Standing::Closed
                    | Standing::OutOfScope,
                )
                | None => Vec::new(),
            };
            let dependents = tickets
                .values()
                .filter(|other| other.blocked_by.contains(id))
                .map(|other| other.id.clone())
                .collect();
            (
                id.clone(),
                Relations {
                    open_blockers,
                    dependents,
                },
            )
        })
        .collect()
}

fn problem(ticket: &Ticket, line: usize, message: String) -> Problem {
    Problem {
        file: format!("{}.md", ticket.id),
        line,
        message,
    }
}

/// What is wrong across the tickets and against the map. `unreadable`
/// names the ticket files that could not be parsed into a ticket, so a
/// reference to one is not also reported as dangling. `existing_repos` is
/// the set of named repositories that exist under the root.
pub fn check(
    tickets: &BTreeMap<String, Ticket>,
    unreadable: &BTreeSet<String>,
    map: &MapDoc,
    existing_repos: &BTreeSet<String>,
) -> Vec<Problem> {
    let linked = |section: &str, id: &str| {
        let link = format!("({id}.md)");
        section.lines().any(|line| line.contains(&link))
    };
    let mut problems = Vec::new();
    for ticket in tickets.values() {
        for blocker in &ticket.blocked_by {
            match tickets.get(blocker) {
                None if unreadable.contains(blocker) => {}
                None => problems.push(problem(
                    ticket,
                    ticket.blocked_by_line,
                    format!("blocked_by names `{blocker}`, which is not a ticket here"),
                )),
                Some(other)
                    if ticket.status == Status::Open && other.status == Status::OutOfScope =>
                {
                    problems.push(problem(
                        ticket,
                        ticket.blocked_by_line,
                        format!("open ticket is blocked by out-of-scope ticket `{blocker}`"),
                    ));
                }
                Some(_) => {}
            }
        }
        for repo in ticket.repos.iter().flatten() {
            if !existing_repos.contains(repo) {
                problems.push(problem(
                    ticket,
                    ticket.repos_line,
                    format!("repos names `{repo}`, which is not a git repository under the root"),
                ));
            }
        }
        match ticket.status {
            Status::Closed => {
                if !ticket.has_resolution {
                    problems.push(problem(
                        ticket,
                        ticket.status_line,
                        "closed ticket has no `## Resolution`".to_string(),
                    ));
                }
                if !linked(&map.decisions, &ticket.id) {
                    problems.push(problem(
                        ticket,
                        ticket.status_line,
                        format!(
                            "closed ticket is not linked from `## Decisions so far` in {MAP_FILE}: \
                             a line must contain `({}.md)`",
                            ticket.id
                        ),
                    ));
                }
            }
            Status::OutOfScope if !linked(&map.out_of_scope, &ticket.id) => {
                problems.push(problem(
                    ticket,
                    ticket.status_line,
                    format!(
                        "out-of-scope ticket is not linked from `## Out of scope` in {MAP_FILE}: \
                         a line must contain `({}.md)`",
                        ticket.id
                    ),
                ));
            }
            Status::Open | Status::OutOfScope => {}
        }
    }
    problems.extend(cycles(tickets));
    problems
}

/// A ticket id's leading `<letters><digits>` when the id is that and then
/// ends or continues with `-`: the letters and the digits.
fn id_prefix(id: &str) -> Option<(&str, &str)> {
    let letters_end = id
        .find(|c: char| !c.is_ascii_alphabetic())
        .filter(|end| *end > 0)?;
    let rest = &id[letters_end..];
    let digits_len = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits_len == 0 {
        return None;
    }
    let after = &rest[digits_len..];
    (after.is_empty() || after.starts_with('-')).then(|| (&id[..letters_end], &rest[..digits_len]))
}

/// Where a ticket id checked by [`duplicate_ids`] comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Listing<'a> {
    /// A ticket file of the directory being checked.
    Branch,
    /// A ticket the named ref added since the merge-base, absent from the
    /// branch.
    Ref(&'a str),
}

/// One problem per ticket whose id shares its leading `<letters><digits>`
/// prefix with another ticket's, on line 1 of its file. `own` and
/// `from_ref` are ticket ids (file stems) in the directory; an id in both
/// counts once. Only a branch ticket is reported: a ref's ticket is a
/// number to avoid, never a line to fix. Of several branch tickets sharing
/// a prefix, the first (by id) stays and each later one is reported; when
/// the ref took the prefix, every branch ticket is. `dir` prefixes the
/// other ticket's path in the message, and `rev` names the ref a ref-only
/// ticket is on. The digits are compared as written, so `f011` and `f11`
/// are different ids.
pub fn duplicate_ids(
    own: &BTreeSet<String>,
    from_ref: Option<(&BTreeSet<String>, &str)>,
    dir: &Path,
) -> Vec<Problem> {
    let mut tickets: Vec<(&str, Listing)> = own
        .iter()
        .map(|id| (id.as_str(), Listing::Branch))
        .collect();
    if let Some((ref_ids, rev)) = from_ref {
        tickets.extend(
            ref_ids
                .iter()
                .filter(|id| !own.contains(*id))
                .map(|id| (id.as_str(), Listing::Ref(rev))),
        );
    }
    tickets.sort_unstable();
    let mut groups: BTreeMap<(&str, &str), Vec<(&str, Listing)>> = BTreeMap::new();
    let mut highest: BTreeMap<&str, Option<u64>> = BTreeMap::new();
    for &(id, origin) in &tickets {
        let Some((letters, digits)) = id_prefix(id) else {
            continue;
        };
        // A number too long for u64 leaves the next free one unknown.
        let slot = highest.entry(letters).or_insert(Some(0));
        *slot = match (*slot, digits.parse::<u64>()) {
            (Some(high), Ok(number)) => Some(high.max(number)),
            _ => None,
        };
        groups
            .entry((letters, digits))
            .or_default()
            .push((id, origin));
    }
    let mut problems = Vec::new();
    for (&(letters, digits), group) in &groups {
        if group.len() < 2 {
            continue;
        }
        let ref_took_it = group
            .iter()
            .any(|(_, origin)| matches!(origin, Listing::Ref(_)));
        let mut first_branch_seen = false;
        for &(id, origin) in group {
            if matches!(origin, Listing::Ref(_)) {
                continue;
            }
            if !first_branch_seen && !ref_took_it {
                first_branch_seen = true;
                continue;
            }
            // The ref's ticket when the ref took the number, else the
            // branch ticket that stays.
            let Some((other, other_origin)) =
                group
                    .iter()
                    .copied()
                    .find(|&(candidate, candidate_origin)| {
                        candidate != id
                            && (!ref_took_it || matches!(candidate_origin, Listing::Ref(_)))
                    })
            else {
                continue;
            };
            let other_path = dir.join(format!("{other}.md"));
            let other_named = match other_origin {
                Listing::Ref(rev) => format!("{} on {rev}", other_path.display()),
                Listing::Branch => other_path.display().to_string(),
            };
            // Each suggestion takes the next free number, so following
            // every hint never makes a new duplicate.
            let slot = highest.entry(letters).or_insert(None);
            let next = slot.and_then(|high| high.checked_add(1));
            if next.is_some() {
                *slot = next;
            }
            let renumber = match next {
                Some(next) => format!("; renumber to {letters}{next}"),
                None => String::new(),
            };
            problems.push(Problem {
                file: format!("{id}.md"),
                line: 1,
                message: format!("id {letters}{digits} is also used by {other_named}{renumber}"),
            });
        }
    }
    problems
}

/// What is worth fixing but does not fail `check`: each open afk ticket
/// without a `## Look at` or a `## Answer looks like` section, on its
/// heading line.
pub fn warnings(tickets: &BTreeMap<String, Ticket>) -> Vec<Problem> {
    let mut found = Vec::new();
    for ticket in tickets.values() {
        if ticket.status != Status::Open || ticket.mode != Mode::Afk {
            continue;
        }
        for (present, title) in [
            (ticket.has_look_at, "## Look at"),
            (ticket.has_answer_looks_like, "## Answer looks like"),
        ] {
            if !present {
                found.push(problem(
                    ticket,
                    ticket.heading_line,
                    format!("open afk ticket has no `{title}` section"),
                ));
            }
        }
    }
    found
}

/// One problem per cycle, on its smallest ticket id, naming the path round.
// holds-until: ~1000 tickets in one programme — one reachability walk per
// ticket is quadratic; a strongly-connected-components pass after
fn cycles(tickets: &BTreeMap<String, Ticket>) -> Vec<Problem> {
    let mut reported: BTreeSet<&str> = BTreeSet::new();
    let mut problems = Vec::new();
    for start in tickets.values() {
        if reported.contains(start.id.as_str()) {
            continue;
        }
        let Some(path) = path_back_to(tickets, &start.id) else {
            continue;
        };
        let members = members_of_cycle_through(tickets, &start.id);
        reported.extend(members);
        problems.push(problem(
            start,
            start.blocked_by_line,
            format!("blocked_by cycle: {}", path.join(" -> ")),
        ));
    }
    problems
}

/// The tickets reachable from `start` by one or more `blocked_by` edges,
/// each with the ticket it was reached from.
fn reach<'a>(tickets: &'a BTreeMap<String, Ticket>, start: &'a str) -> BTreeMap<&'a str, &'a str> {
    let mut parents: BTreeMap<&str, &str> = BTreeMap::new();
    let mut queue = std::collections::VecDeque::from([start]);
    while let Some(current) = queue.pop_front() {
        let Some(ticket) = tickets.get(current) else {
            continue;
        };
        for next in &ticket.blocked_by {
            if !parents.contains_key(next.as_str()) {
                parents.insert(next, current);
                queue.push_back(next);
            }
        }
    }
    parents
}

/// `start -> ... -> start` when `start` blocks itself through others.
fn path_back_to(tickets: &BTreeMap<String, Ticket>, start: &str) -> Option<Vec<String>> {
    let parents = reach(tickets, start);
    parents.get(start)?;
    let mut path = vec![start.to_string()];
    let mut current = parents[start];
    while current != start {
        path.push(current.to_string());
        current = parents[current];
    }
    path.push(start.to_string());
    path.reverse();
    Some(path)
}

fn members_of_cycle_through<'a>(
    tickets: &'a BTreeMap<String, Ticket>,
    start: &'a str,
) -> Vec<&'a str> {
    let from_start = reach(tickets, start);
    from_start
        .keys()
        .copied()
        .filter(|other| reach(tickets, other).contains_key(start))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::programme::claim::rules::{Holder as ClaimHolder, Kind};
    use crate::programme::model::{Mode, TicketType};

    fn ids(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// The other ticket's path as the message shows it: `dir` joined
    /// with the native separator, so the tests hold on Windows too.
    fn shown(id: &str) -> String {
        Path::new("d")
            .join(format!("{id}.md"))
            .display()
            .to_string()
    }

    /// `(file, message)` of each duplicate found, in order.
    fn duplicates(own: &[&str], from_ref: &[&str]) -> Vec<(String, String)> {
        duplicate_ids(
            &ids(own),
            Some((&ids(from_ref), "origin/main")),
            Path::new("d"),
        )
        .into_iter()
        .map(|problem| (problem.file, problem.message))
        .collect()
    }

    #[test]
    fn id_prefix_takes_letters_then_digits_up_to_a_dash_or_the_end() {
        let cases: &[(&str, Option<(&str, &str)>)] = &[
            ("f12", Some(("f", "12"))),
            ("f12-a", Some(("f", "12"))),
            ("t011-x", Some(("t", "011"))),
            ("map", None),
            ("12-a", None),
            ("f12a", None),
            ("f-12", None),
        ];
        for (id, expected) in cases {
            assert_eq!(id_prefix(id), *expected, "{id}");
        }
    }

    #[test]
    fn of_three_branch_tickets_the_first_stays_and_the_later_two_name_it() {
        assert_eq!(
            duplicates(&["f12-a", "f12-b", "f12-c", "f3-x"], &[]),
            vec![
                (
                    "f12-b.md".to_string(),
                    format!("id f12 is also used by {}; renumber to f13", shown("f12-a"))
                ),
                (
                    "f12-c.md".to_string(),
                    format!("id f12 is also used by {}; renumber to f14", shown("f12-a"))
                ),
            ]
        );
    }

    #[test]
    fn a_number_the_ref_took_reports_every_branch_ticket_against_the_ref() {
        assert_eq!(
            duplicates(&["f71-b"], &["f71-a", "f72-z"]),
            vec![(
                "f71-b.md".to_string(),
                format!(
                    "id f71 is also used by {} on origin/main; renumber to f73",
                    shown("f71-a")
                )
            )]
        );
    }

    #[test]
    fn duplicates_among_ref_tickets_alone_and_padded_numbers_are_not_reported() {
        assert!(duplicates(&["f5-x"], &["f9-a", "f9-b"]).is_empty());
        assert!(duplicates(&["f011-a", "f11-b"], &[]).is_empty());
    }

    #[test]
    fn a_number_too_long_for_u64_drops_the_renumber_hint() {
        let huge = "f99999999999999999999";
        assert_eq!(
            duplicates(&[&format!("{huge}-a"), &format!("{huge}-b")], &[]),
            vec![(
                format!("{huge}-b.md"),
                format!("id {huge} is also used by {}", shown(&format!("{huge}-a")))
            )]
        );
    }

    fn ticket(id: &str, status: Status, blocked_by: &[&str]) -> Ticket {
        Ticket {
            id: id.to_string(),
            name: format!("Ticket {id}"),
            kind: TicketType::Task,
            mode: Mode::Afk,
            status,
            blocked_by: blocked_by.iter().map(|b| b.to_string()).collect(),
            repos: None,
            has_question: true,
            has_look_at: false,
            has_answer_looks_like: false,
            has_resolution: status == Status::Closed,
            status_line: 4,
            blocked_by_line: 5,
            repos_line: 6,
            due: None,
            due_line: 7,
            heading_line: 8,
        }
    }

    fn set(tickets: Vec<Ticket>) -> BTreeMap<String, Ticket> {
        tickets.into_iter().map(|t| (t.id.clone(), t)).collect()
    }

    #[test]
    fn standings_follow_blockers_and_branches() {
        let tickets = set(vec![
            ticket("A", Status::Closed, &[]),
            ticket("B", Status::Open, &["A"]),
            ticket("C", Status::Open, &["D"]),
            ticket("D", Status::Open, &[]),
            ticket("E", Status::OutOfScope, &[]),
        ]);
        let working = BTreeMap::from([("D".to_string(), 1_000)]);
        let claims = BTreeMap::from([
            (
                "B".to_string(),
                Hold {
                    holder: ClaimHolder::Session {
                        id: "s1".to_string(),
                        kind: Kind::Work,
                    },
                    since: 5,
                },
            ),
            (
                "D".to_string(),
                Hold {
                    holder: ClaimHolder::Session {
                        id: "s2".to_string(),
                        kind: Kind::Handoff,
                    },
                    since: 6,
                },
            ),
            (
                "A".to_string(),
                Hold {
                    holder: ClaimHolder::Session {
                        id: "s3".to_string(),
                        kind: Kind::Work,
                    },
                    since: 7,
                },
            ),
        ]);
        let all = standings(&tickets, "demo", &working, &claims);
        assert_eq!(
            all["B"],
            Standing::InFlight {
                holder: Holder::Claim(claims["B"].clone())
            }
        );
        assert_eq!(all["C"], Standing::Blocked(vec!["D".to_string()]));
        assert_eq!(
            all["D"],
            Standing::InFlight {
                holder: Holder::Branch {
                    branch: "prog/demo/D".to_string(),
                    last_commit_unix: 1_000
                }
            },
            "a branch wins over a claim"
        );
        assert_eq!(all["A"], Standing::Closed, "a closed ticket keeps no claim");
        let total = counts(&all);
        assert_eq!(
            (total.open, total.frontier, total.blocked, total.in_flight),
            (3, 0, 1, 2)
        );
    }

    #[test]
    fn only_open_afk_tickets_lacking_a_section_are_warned_about() {
        let mut complete = ticket("A", Status::Open, &[]);
        complete.has_look_at = true;
        complete.has_answer_looks_like = true;
        let mut half = ticket("B", Status::Open, &[]);
        half.has_look_at = true;
        let mut hitl = ticket("C", Status::Open, &[]);
        hitl.mode = Mode::Hitl;
        let tickets = set(vec![
            complete,
            half,
            hitl,
            ticket("D", Status::Closed, &[]),
            ticket("E", Status::OutOfScope, &[]),
            ticket("F", Status::Open, &[]),
        ]);
        let found: Vec<(String, String)> = warnings(&tickets)
            .into_iter()
            .map(|w| (w.file, w.message))
            .collect();
        assert_eq!(
            found,
            [
                (
                    "B.md",
                    "open afk ticket has no `## Answer looks like` section"
                ),
                ("F.md", "open afk ticket has no `## Look at` section"),
                (
                    "F.md",
                    "open afk ticket has no `## Answer looks like` section"
                ),
            ]
            .map(|(f, m)| (f.to_string(), m.to_string()))
        );
    }

    #[test]
    fn an_age_is_clamped_at_zero() {
        assert_eq!(age_secs(1_000, 400), 600);
        assert_eq!(age_secs(1_000, 1_500), 0);
    }

    #[test]
    fn a_cycle_is_one_problem_on_its_smallest_id() {
        let tickets = set(vec![
            ticket("A", Status::Open, &["B"]),
            ticket("B", Status::Open, &["C"]),
            ticket("C", Status::Open, &["A"]),
            ticket("D", Status::Open, &["D"]),
        ]);
        let found = cycles(&tickets);
        let messages: Vec<&str> = found.iter().map(|p| p.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "blocked_by cycle: A -> B -> C -> A",
                "blocked_by cycle: D -> D"
            ]
        );
    }
}
