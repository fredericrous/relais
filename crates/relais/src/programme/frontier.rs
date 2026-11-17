//! The ready tickets of a repository's programmes, in the order
//! `relais programme frontier` lists them (SPEC §31): shared by that
//! command and by the SessionStart hook, so the two never disagree about
//! which ticket comes first. Nothing here prints.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::NaiveDate;

use super::analysis::Standing;
use super::load::{Found, Programme};
use super::model::Ticket;

/// Whether a ticket in this standing is ready: open and not blocked, in
/// flight or out of scope. The one place that says so.
pub fn is_ready(standing: &Standing) -> bool {
    matches!(standing, Standing::Frontier)
}

/// The ready tickets of `programme`, in ticket order.
pub fn ready_tickets<'a>(
    programme: &'a Programme,
    standings: &'a BTreeMap<String, Standing>,
) -> impl Iterator<Item = &'a Ticket> {
    programme
        .tickets
        .values()
        .filter(|ticket| standings.get(&ticket.id).is_some_and(is_ready))
}

/// The frontier's order: tickets with a due date first, soonest first, then
/// the undated ones; the sort is stable, so ties keep the tickets' id order.
pub fn order_frontier<T>(rows: &mut [T], due_of: impl Fn(&T) -> Option<NaiveDate>) {
    rows.sort_by_key(|row| {
        let due = due_of(row);
        (due.is_none(), due)
    });
}

/// Whether a due date has passed on `today`: the same day is still due.
pub fn is_overdue(due: NaiveDate, today: NaiveDate) -> bool {
    due < today
}

/// `[due YYYY-MM-DD] ` or `[overdue YYYY-MM-DD] `, empty when undated.
pub fn due_mark(due: Option<NaiveDate>, today: NaiveDate) -> String {
    match due {
        None => String::new(),
        Some(date) if is_overdue(date, today) => format!("[overdue {date}] "),
        Some(date) => format!("[due {date}] "),
    }
}

/// One ready ticket, and which programme (by position in [`Ready::found`])
/// it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyRow {
    pub programme: usize,
    pub slug: String,
    pub id: String,
    pub name: String,
    pub due: Option<NaiveDate>,
}

/// Every ready ticket of some programmes, ordered, with the source each
/// programme was selected from kept beside it.
#[derive(Debug)]
pub struct Ready {
    pub found: Vec<Found>,
    pub rows: Vec<ReadyRow>,
    /// The directory of each programme that failed to load, in order.
    pub unreadable: Vec<PathBuf>,
}

/// The ready tickets of `found`, which are expected in slug order: the
/// frontier of each loaded programme, due ones first across all of them,
/// then the undated by programme and id. A programme that does not load
/// is counted in [`Ready::unreadable`], never fatal; `unlisted` are the
/// directories of programmes that could not even be selected, which are
/// counted the same way, before the ones that failed to load.
pub fn ready(found: Vec<Found>, unlisted: Vec<PathBuf>) -> Ready {
    let mut rows = Vec::new();
    let mut unreadable = unlisted;
    // Each load spawns several git processes (about 40 ms each); one thread
    // per programme keeps a repository with many programmes inside the
    // session-start deadline. Results are read back in `found`'s order.
    // holds-until: ~9 programmes, measured in decisions with 7 worktrees —
    // a bounded pool once a repository holds enough programmes that one
    // thread each outruns the session-start deadline
    let loads: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = found
            .iter()
            .map(|one| {
                scope.spawn(move || {
                    one.load()
                        .and_then(|programme| programme.standings().map(|s| (programme, s)))
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join()).collect()
    });
    for ((position, one), loaded) in found.iter().enumerate().zip(loads) {
        // A load that panicked counts as one that failed.
        match loaded.ok().and_then(Result::ok) {
            Some((programme, standings)) => {
                for ticket in ready_tickets(&programme, &standings) {
                    rows.push(ReadyRow {
                        programme: position,
                        slug: one.slug.clone(),
                        id: ticket.id.clone(),
                        name: ticket.name.clone(),
                        due: ticket.due,
                    });
                }
            }
            None => unreadable.push(one.dir()),
        }
    }
    order_frontier(&mut rows, |row| row.due);
    Ready {
        found,
        rows,
        unreadable,
    }
}

/// Whether a map has a `## Handoff` heading outside a fenced code block.
pub fn has_handoff(map_text: &str) -> bool {
    let mut fence: Option<(char, usize)> = None;
    for line in map_text.lines() {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~');
        if let Some(marker) = marker {
            let run = trimmed.chars().take_while(|c| *c == marker).count();
            if run >= 3 && line.len() - trimmed.len() <= 3 {
                match fence {
                    None => fence = Some((marker, run)),
                    Some((open, width)) if open == marker && run >= width => fence = None,
                    Some(_) => {}
                }
                continue;
            }
        }
        if fence.is_none() && line.trim_end() == "## Handoff" {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handoff_heading_inside_a_fence_is_not_one() {
        assert!(has_handoff("# M\n\n## Handoff\n\ntext\n"));
        assert!(!has_handoff("# M\n\n```\n## Handoff\n```\n"));
        assert!(!has_handoff("# M\n\n~~~md\n## Handoff\n~~~\n"));
        assert!(has_handoff("```\nx\n```\n## Handoff\n"));
        assert!(!has_handoff("## Handoffs\n### Handoff\n"));
    }
}
