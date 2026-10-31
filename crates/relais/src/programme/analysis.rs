//! Pure judgement over parsed tickets: where each stands, and what is
//! wrong between them. No filesystem, no git: the branches that work
//! tickets and the repositories that exist arrive as values.

use std::collections::{BTreeMap, BTreeSet};

use super::model::{MapDoc, Problem, Status, Ticket, MAP_FILE};

/// Where a ticket stands against the others and the branches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Open, every blocker closed, no branch works it.
    Frontier,
    /// Open, and these tickets (not closed, or not there) block it.
    Blocked(Vec<String>),
    /// Open, and this branch works it.
    InFlight(String),
    Closed,
    OutOfScope,
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

/// Every ticket's standing. `working` holds the ids that a
/// `prog/<slug>/<id>` branch works; a branch wins over blockers because it
/// is the stronger fact about the ticket.
pub fn standings(
    tickets: &BTreeMap<String, Ticket>,
    slug: &str,
    working: &BTreeSet<String>,
) -> BTreeMap<String, Standing> {
    tickets
        .values()
        .map(|ticket| {
            let standing = match ticket.status {
                Status::Closed => Standing::Closed,
                Status::OutOfScope => Standing::OutOfScope,
                Status::Open if working.contains(&ticket.id) => {
                    Standing::InFlight(branch_name(slug, &ticket.id))
                }
                Status::Open => {
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
            Standing::InFlight(_) => counts.in_flight += 1,
            Standing::Closed => counts.closed += 1,
            Standing::OutOfScope => counts.out_of_scope += 1,
        }
    }
    counts.open = counts.frontier + counts.blocked + counts.in_flight;
    counts
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
    use crate::programme::model::{Mode, TicketType};

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
            has_resolution: status == Status::Closed,
            status_line: 4,
            blocked_by_line: 5,
            repos_line: 6,
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
        let working = BTreeSet::from(["D".to_string()]);
        let all = standings(&tickets, "demo", &working);
        assert_eq!(all["B"], Standing::Frontier);
        assert_eq!(all["C"], Standing::Blocked(vec!["D".to_string()]));
        assert_eq!(all["D"], Standing::InFlight("prog/demo/D".to_string()));
        let total = counts(&all);
        assert_eq!(
            (total.open, total.frontier, total.blocked, total.in_flight),
            (3, 1, 1, 1)
        );
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
