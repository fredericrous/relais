//! The ready programme tickets a session that starts empty is told about
//! (SPEC §23, §31): `gather` reads them, `render` writes them.
//!
//! Reading is I/O and lives in [`gather`], which `respond` runs on a
//! worker thread under a deadline. [`render`] is pure over what was read,
//! so each cap and each mark has a unit test.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::NaiveDate;

use super::respond::{Coverage, Gathered, Skip, SkipReason, GATHER_THREAD_NAME};
use crate::programme::claim::{self, Store};
use crate::programme::frontier::{self, due_mark, format_age, has_handoff, Ready, ReadyRow};
use crate::programme::load::{self, main_checkout, Found, Source};
use crate::programme::model::MAP_FILE;

/// Ticket lines shown before the rest are counted.
pub const TICKET_LINE_CAP: usize = 8;

/// Characters of a ticket name shown before it is cut with `…`.
pub const NAME_CAP: usize = 200;

/// Programmes whose `## Handoff` is pointed at.
pub const HANDOFF_POINTER_CAP: usize = 3;

/// The version of the contract that a map's `## Handoff` heading is read
/// under (SPEC §31): the heading's name is what relais reads, nothing in
/// its body.
pub const HANDOFF_CONTRACT: &str = "relais-handoff/1";

/// How long a session start outside any repository reads its child
/// repositories before it renders what arrived. It is counted from the
/// entry of [`gather`], so discovery and spawning spend the same budget, and
/// it stays under `respond::GATHER_DEADLINE`.
pub const OUTSIDE_REPO_DEADLINE: Duration = Duration::from_millis(2500);

/// The context text for a session that starts in `cwd`, or why there is
/// none: [`SkipReason::Nothing`] when its repository holds no programme or
/// nothing is ready, [`SkipReason::Error`] with what failed when the
/// repository or its programmes cannot be listed. A session start is never failed by this.
/// Reads checkouts and origin as the last fetch left them; fetches nothing
/// and prints nothing.
///
/// When `cwd` is not inside a repository, the child repositories under it
/// are read instead (see [`render_outside`]).
pub fn gather(cwd: &Path) -> Result<Gathered, Skip> {
    gather_with(cwd, Store::from_environment())
}

/// [`gather`] marking the tickets that `claims` holds. Reads claims and
/// never writes one.
pub fn gather_with(cwd: &Path, claims: Option<Store>) -> Result<Gathered, Skip> {
    let cutoff = Instant::now() + OUTSIDE_REPO_DEADLINE;
    let repo = match main_checkout(cwd) {
        Ok(repo) => repo,
        Err(not_a_checkout) => {
            return gather_outside(cwd, cutoff, claims).map_err(|skip| {
                // A broken git looks like no checkout at all: keep its cause,
                // first, so a cut of a long text keeps it.
                let error = skip
                    .error
                    .map(|text| format!("not a checkout: {not_a_checkout}; {text}"));
                Skip::from_parts(skip.why, error)
            });
        }
    };
    let listed = load::programmes_of(&repo).map_err(|error| Skip::error(error.to_string()))?;
    let ready = frontier::ready(
        listed.found,
        listed.unreadable,
        claims.as_ref(),
        claim::now_unix(),
    );
    let today = chrono::Local::now().date_naive();
    render(
        &repo,
        &ready,
        today,
        |found| found.map_text().is_ok_and(|text| has_handoff(&text)),
        None,
    )
    .map(Gathered::from)
    .ok_or(SkipReason::Nothing.into())
}

fn gather_outside(root: &Path, cutoff: Instant, claims: Option<Store>) -> Result<Gathered, Skip> {
    let repos =
        load::repositories_with_programmes(root).map_err(|error| Skip::error(error.to_string()))?;
    if repos.is_empty() {
        // Child checkouts that hold no programme say nothing; no checkout at
        // all is an error.
        let children = load::repositories(root).map_err(|error| Skip::error(error.to_string()))?;
        return Err(if children.is_empty() {
            Skip::error(format!("no git repository under {}", root.display()))
        } else {
            SkipReason::Nothing.into()
        });
    }
    let collected = collect_within(
        repos,
        move |repo| read_repository(repo, claims.as_ref()),
        cutoff,
    );
    let today = chrono::Local::now().date_naive();
    let text = render_outside(root, &collected, today)?;
    Ok(Gathered {
        text,
        coverage: Some(Coverage {
            read: collected.read.len(),
            unread: collected.late.len() + collected.unreadable.len(),
        }),
    })
}

/// What one thread read of one child repository.
#[derive(Debug)]
pub(crate) struct RepoReading {
    pub(crate) repo: PathBuf,
    pub(crate) ready: Ready,
    /// The slugs of the programmes with a `## Handoff`, read where the
    /// thread ran so that nothing runs git after the cutoff.
    pub(crate) handoffs: BTreeSet<String>,
    /// The programme whose map was handed off last, when any was.
    pub(crate) latest_handoff: Option<LatestHandoff>,
}

/// The newest `## Handoff` of a repository: its programme's slug and its
/// map's time in Unix seconds, `None` when no time could be read (which
/// ranks below every known time).
#[derive(Debug, Clone)]
pub(crate) struct LatestHandoff {
    pub(crate) slug: String,
    pub(crate) at: Option<i64>,
}

/// Why the programmes of `repo` could not be read.
fn read_repository(repo: &Path, claims: Option<&Store>) -> Result<RepoReading, String> {
    let listed = load::programmes_of(repo).map_err(|error| error.to_string())?;
    let mut handoffs = BTreeSet::new();
    let mut latest_handoff: Option<LatestHandoff> = None;
    for found in &listed.found {
        if !found.map_text().is_ok_and(|text| has_handoff(&text)) {
            continue;
        }
        handoffs.insert(found.slug.clone());
        let at = load::map_time(found);
        if latest_handoff.as_ref().is_none_or(|latest| at > latest.at) {
            latest_handoff = Some(LatestHandoff {
                slug: found.slug.clone(),
                at,
            });
        }
    }
    Ok(RepoReading {
        repo: repo.to_path_buf(),
        ready: frontier::ready(listed.found, listed.unreadable, claims, claim::now_unix()),
        handoffs,
        latest_handoff,
    })
}

/// What [`collect_within`] gathered by its cutoff.
#[derive(Debug)]
pub(crate) struct Collected<R> {
    /// The repositories read in time, in the order they were given.
    pub(crate) read: Vec<R>,
    /// The repositories still being read at the cutoff.
    pub(crate) late: Vec<PathBuf>,
    /// The repositories whose reading failed, panicked or could not start,
    /// each with the reason.
    pub(crate) unreadable: Vec<(PathBuf, String)>,
}

/// Read every repository of `repos` with `read`, each on its own detached
/// thread, and take what has arrived by `cutoff`. Never `thread::scope`,
/// which would join past the cutoff: a thread still running is left to
/// finish, as in `respond::gather_within`. `read` answers the reason for a
/// repository it could not read.
// holds-until: ~60 child repositories — a bounded pool once one thread each
// outruns the cores of a machine that checks out many more
pub(crate) fn collect_within<R, F>(repos: Vec<PathBuf>, read: F, cutoff: Instant) -> Collected<R>
where
    R: Send + 'static,
    F: Fn(&Path) -> Result<R, String> + Send + Sync + 'static,
{
    let read = Arc::new(read);
    let (sender, receiver) = mpsc::channel();
    let mut arrived: Vec<Option<Result<R, String>>> = repos.iter().map(|_| None).collect();
    for (index, repo) in repos.iter().enumerate() {
        let (read, sender, owned) = (Arc::clone(&read), sender.clone(), repo.clone());
        let spawned = std::thread::Builder::new()
            .name(GATHER_THREAD_NAME.into())
            .spawn(move || {
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(&owned)))
                        .unwrap_or_else(|_| Err("its reader panicked".to_string()));
                let _ = sender.send((index, outcome)); // the receiver is gone once the cutoff passed
            });
        if spawned.is_err() {
            // the repository is recorded unreadable and the others still run
            arrived[index] = Some(Err("its reader thread could not start".to_string()));
        }
    }
    drop(sender);
    while let Ok((index, outcome)) =
        receiver.recv_timeout(cutoff.saturating_duration_since(Instant::now()))
    {
        arrived[index] = Some(outcome);
    }
    let mut collected = Collected {
        read: Vec::new(),
        late: Vec::new(),
        unreadable: Vec::new(),
    };
    for (repo, outcome) in repos.into_iter().zip(arrived) {
        match outcome {
            Some(Ok(reading)) => collected.read.push(reading),
            Some(Err(reason)) => collected.unreadable.push((repo, reason)),
            None => collected.late.push(repo),
        }
    }
    collected
}

/// Summary lines are listed up to [`TICKET_LINE_CAP`]; names of unread
/// repositories up to this many.
const UNREAD_NAME_CAP: usize = 5;

/// The repository names of `repos`, cut at [`UNREAD_NAME_CAP`] with `…`.
fn name_list<'a>(repos: impl ExactSizeIterator<Item = &'a PathBuf>) -> String {
    let total = repos.len();
    let mut names: Vec<String> = repos
        .take(UNREAD_NAME_CAP)
        .map(|repo| repo_name(repo))
        .collect();
    if total > UNREAD_NAME_CAP {
        names.push("…".into());
    }
    names.join(", ")
}

/// The text for a session that starts outside any repository, from what
/// [`collect_within`] read under `root`: one summary line per repository
/// with ready tickets, then the full block of the repository whose map
/// holds the latest `## Handoff`.
pub(crate) fn render_outside(
    root: &Path,
    collected: &Collected<RepoReading>,
    today: NaiveDate,
) -> Result<String, Skip> {
    if collected.read.is_empty() {
        // Nothing read: a deadline when any repository was still being read,
        // an error when every one of them failed; either keeps the reasons.
        let reasons: Vec<String> = collected
            .unreadable
            .iter()
            .map(|(repo, reason)| format!("{}: {reason}", repo.display()))
            .collect();
        let reasons = (!reasons.is_empty()).then(|| reasons.join("; "));
        let why = if collected.late.is_empty() && reasons.is_some() {
            SkipReason::Error
        } else {
            SkipReason::Deadline
        };
        return Err(Skip::from_parts(why, reasons));
    }
    let mut with_ready: Vec<&RepoReading> = collected
        .read
        .iter()
        .filter(|reading| !reading.ready.rows.is_empty())
        .collect();
    with_ready.sort_by_key(|reading| {
        let soonest = unheld_rows(&reading.ready).filter_map(|row| row.due).min();
        (
            soonest.is_none(),
            soonest,
            std::cmp::Reverse(reading.ready.unheld_count()),
            repo_name(&reading.repo),
        )
    });
    // The expanded repository has the most unheld ready tickets; among
    // those, the latest `## Handoff` wins, a known time over an unknown one.
    let most_unheld = collected
        .read
        .iter()
        .map(|reading| reading.ready.unheld_count())
        .max()
        .unwrap_or(0);
    let handoff_rank =
        |reading: &RepoReading| (reading.latest_handoff.is_some(), handoff_time(reading));
    let expanded_choice = with_ready
        .iter()
        .copied()
        .chain(collected.read.iter().filter(|r| r.ready.rows.is_empty()))
        .filter(|reading| reading.ready.unheld_count() == most_unheld)
        .filter(|reading| most_unheld > 0 || reading.latest_handoff.is_some())
        .fold(None::<&RepoReading>, |kept, candidate| match kept {
            Some(best) if handoff_rank(best) >= handoff_rank(candidate) => kept,
            _ => Some(candidate),
        });
    let first_with_ready = with_ready.first().copied();
    let (expanded, label, pinned) = match expanded_choice.or(first_with_ready) {
        Some(reading) => {
            let handoff = reading.latest_handoff.as_ref();
            (
                reading,
                if handoff.is_some() {
                    "latest ## Handoff among those read"
                } else {
                    "no repository has a ## Handoff"
                },
                handoff.and_then(|handoff| {
                    reading
                        .ready
                        .found
                        .iter()
                        .find(|found| found.slug == handoff.slug)
                }),
            )
        }
        None => return Err(SkipReason::Nothing.into()),
    };
    let quoted_root = shell_quote(&printable(&root.display().to_string()));

    let mut text = String::new();
    if !with_ready.is_empty() {
        let total: usize = with_ready.iter().map(|r| r.ready.unheld_count()).sum();
        let held: usize = with_ready.iter().map(|r| r.ready.held_count()).sum();
        text.push_str(&format!(
            "relais/programmes: {total} ready{} in {} under {quoted_root} (not a repository):\n",
            held_count_suffix(held),
            plural(with_ready.len(), "repository", "repositories"),
        ));
        for reading in with_ready.iter().take(TICKET_LINE_CAP) {
            text.push_str(&summary_line(reading));
        }
        if with_ready.len() > TICKET_LINE_CAP {
            let overflow = with_ready.len() - TICKET_LINE_CAP;
            text.push_str(&format!(
                "- … and {} more {} (relais programme list --no-fetch --root {quoted_root} lists them)\n",
                overflow,
                if overflow == 1 { "repository" } else { "repositories" },
            ));
        }
    }
    if !collected.late.is_empty() {
        let count = collected.late.len();
        text.push_str(&format!(
            "({} not read in time: {}; relais programme list --no-fetch --root {quoted_root} lists {})\n",
            plural(count, "repository", "repositories"),
            name_list(collected.late.iter()),
            if count == 1 { "it" } else { "them" }
        ));
    }
    // `relais programme check` takes one programme directory, not a
    // repository, so the line carries the first reason itself and names the
    // command that reads every repository under the root.
    if let Some((_, first_reason)) = collected.unreadable.first() {
        text.push_str(&format!(
            "({} could not be read: {} (first: {}); relais programme list --no-fetch --root {quoted_root} says why)\n",
            plural(collected.unreadable.len(), "repository", "repositories"),
            name_list(collected.unreadable.iter().map(|(repo, _)| repo)),
            printable(first_reason),
        ));
    }
    text.push_str(&format!(
        "expanded: {} ({label})\n",
        repo_name(&expanded.repo)
    ));
    let block = render(
        &expanded.repo,
        &expanded.ready,
        today,
        |found| expanded.handoffs.contains(&found.slug),
        pinned,
    );
    text.push_str(&block.unwrap_or_default());
    Ok(text)
}

/// The time a repository's latest `## Handoff` was written; `None` ranks
/// below every known time.
fn handoff_time(reading: &RepoReading) -> Option<i64> {
    reading.latest_handoff.as_ref().and_then(|latest| latest.at)
}

fn unheld_rows(ready: &Ready) -> impl Iterator<Item = &ReadyRow> {
    ready.rows.iter().filter(|row| row.held.is_none())
}

/// `, 2 held` after a ready count, nothing when no ticket is held.
fn held_count_suffix(held: usize) -> String {
    if held == 0 {
        String::new()
    } else {
        format!(", {held} held")
    }
}

fn summary_line(reading: &RepoReading) -> String {
    let dated: Vec<NaiveDate> = unheld_rows(&reading.ready)
        .filter_map(|row| row.due)
        .collect();
    let due = match dated.iter().min() {
        Some(first) => format!(", {} due (first {first})", dated.len()),
        None => String::new(),
    };
    format!(
        "- {}: {} ready{}{due}; cd {}\n",
        repo_name(&reading.repo),
        reading.ready.unheld_count(),
        held_count_suffix(reading.ready.held_count()),
        shell_quote(&printable(&reading.repo.display().to_string()))
    )
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The last path component of a repository, printable on one line.
fn repo_name(repo: &Path) -> String {
    printable(&repo.file_name().map_or_else(
        || repo.display().to_string(),
        |n| n.to_string_lossy().into(),
    ))
}

/// `text` with each control character replaced, so a directory name cannot
/// add lines or escape sequences to what the session is told.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Text for the ready tickets, or `None` when there is none. `handoff_in`
/// says whether a programme's selected map has a `## Handoff`; it is asked
/// only of programmes with a shown ticket, at most
/// [`HANDOFF_POINTER_CAP`] of them being named. `pinned` is a programme of
/// `ready` whose `## Handoff` is always pointed at first, even with no
/// shown ticket; with it the text is also written when nothing is ready
/// (`0 ready`). Inside a repository it is `None`.
pub fn render(
    repo: &Path,
    ready: &Ready,
    today: NaiveDate,
    handoff_in: impl Fn(&Found) -> bool,
    pinned: Option<&Found>,
) -> Option<String> {
    if ready.rows.is_empty() && pinned.is_none() {
        return None;
    }
    let mut text = format!(
        "relais/programmes: {} ready{} in {}",
        ready.unheld_count(),
        held_count_suffix(ready.held_count()),
        printable(&repo.display().to_string())
    );
    // `check` and `frontier` each take one programme directory, so with
    // several programmes a line names them all and runs on the first.
    if let Some(first) = ready.unreadable.first() {
        let first = shell_quote(&printable(&first.display().to_string()));
        text.push_str(&match ready.unreadable.len() {
            1 => format!(" (1 programme could not be read; run relais programme check {first} for why)"),
            count => format!(
                " ({count} programmes could not be read: {}; run relais programme check {first} for why on the first, {} more)",
                name_list(ready.unreadable.iter()),
                count - 1
            ),
        });
    }
    text.push_str(":\n");

    // Held rows come after unheld ones, before the cap is applied; the sort
    // is stable, so each group keeps the frontier's order.
    let mut ordered: Vec<&ReadyRow> = ready.rows.iter().collect();
    ordered.sort_by_key(|row| row.held.is_some());
    let (shown, hidden) = ordered.split_at(ordered.len().min(TICKET_LINE_CAP));
    for row in shown {
        text.push_str(&format!(
            "- {} {}: {}{}{}\n",
            row.slug,
            row.id,
            due_mark(row.due, today),
            cut_name(&row.name),
            held_mark(row)
        ));
    }
    if !hidden.is_empty() {
        // In order of first appearance, so "the first" is the first named.
        let mut programmes: Vec<usize> = Vec::new();
        for row in hidden {
            if !programmes.contains(&row.programme) {
                programmes.push(row.programme);
            }
        }
        let dirs: Vec<PathBuf> = programmes
            .iter()
            .map(|&position| ready.found[position].dir())
            .collect();
        let first = shell_quote(&printable(&dirs[0].display().to_string()));
        text.push_str(&match dirs.len() {
            1 => format!(
                "- … and {} more ready (1 programme; relais programme frontier --no-fetch {first} lists them)\n",
                hidden.len()
            ),
            count => format!(
                "- … and {} more ready ({count} programmes: {}; relais programme frontier --no-fetch {first} lists the first, {} more)\n",
                hidden.len(),
                name_list(dirs.iter()),
                count - 1
            ),
        });
    }

    let shown_programmes = programmes_shown(shown);
    if !shown_programmes.is_empty() {
        text.push_str("read a ticket from:\n");
    }
    for (position, first_id) in &shown_programmes {
        let found = &ready.found[*position];
        text.push_str(&format!(
            "- {}: {}\n",
            found.slug,
            ticket_locator(found, first_id)
        ));
    }

    let pointers: Vec<String> = pinned
        .into_iter()
        .chain(
            shown_programmes
                .iter()
                .map(|(position, _)| &ready.found[*position])
                .filter(|found| pinned.is_none_or(|pin| pin.slug != found.slug))
                .filter(|found| handoff_in(found)),
        )
        .take(HANDOFF_POINTER_CAP)
        .map(handoff_line)
        .collect();
    if !pointers.is_empty() {
        text.push_str("handoff:\n");
        pointers.iter().for_each(|pointer| text.push_str(pointer));
    }
    Some(text)
}

/// The programmes with a shown ticket, in order of first appearance, each
/// with the id of its first shown ticket.
fn programmes_shown(shown: &[&ReadyRow]) -> Vec<(usize, String)> {
    let mut seen = BTreeSet::new();
    shown
        .iter()
        .filter(|row| seen.insert(row.programme))
        .map(|row| (row.programme, row.id.clone()))
        .collect()
}

/// ` (held by <s8>, <age>)` after the (possibly cut) name, so the mark is
/// never cut; nothing for an unheld row.
fn held_mark(row: &ReadyRow) -> String {
    match &row.held {
        Some(held) => format!(
            " (held by {}, {})",
            printable(&held.holder.short()),
            format_age(held.age_secs)
        ),
        None => String::new(),
    }
}

fn cut_name(name: &str) -> String {
    if name.chars().count() <= NAME_CAP {
        return name.to_string();
    }
    let kept: String = name.chars().take(NAME_CAP).collect();
    format!("{kept}…")
}

/// The line that points at `found`'s `## Handoff`: the file for a
/// checkout's map, the command to run for a ref's.
fn handoff_line(found: &Found) -> String {
    match &found.source {
        Source::WorkingTree => format!(
            "- {}: the ## Handoff in {} says where the last session stopped ({HANDOFF_CONTRACT})\n",
            found.slug,
            printable(&found.dir().join(MAP_FILE).display().to_string())
        ),
        Source::Ref(rev) => format!(
            "- {}: run {}; its ## Handoff says where the last session stopped ({HANDOFF_CONTRACT})\n",
            found.slug,
            show_command(found, rev, MAP_FILE)
        ),
    }
}

/// Where ticket `id` of `found` is read from: the programme directory for
/// a checkout's files, the `git show` that prints it for a ref, which has
/// no file in any checkout.
fn ticket_locator(found: &Found, id: &str) -> String {
    match &found.source {
        Source::WorkingTree => printable(&found.dir().display().to_string()),
        Source::Ref(rev) => show_command(found, rev, &format!("{id}.md")),
    }
}

fn show_command(found: &Found, rev: &str, file: &str) -> String {
    format!(
        "git -C {} show {}",
        shell_quote(&found.repo.display().to_string()),
        shell_quote(&format!("{rev}:docs/programmes/{}/{file}", found.slug))
    )
}

/// `word` as one shell word: bare when it holds only characters a shell
/// never treats specially, else in single quotes.
fn shell_quote(word: &str) -> String {
    let word = &printable(word);
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%+=,".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::programme::frontier::HeldBy;

    /// A programme directory as `Found::dir` builds it: the checkout joined
    /// with the one relative `docs/programmes/<slug>`, so on Windows only the
    /// join itself is a backslash (`/work/repo\docs/programmes/alpha`).
    fn programme_dir(checkout: &str, slug: &str) -> PathBuf {
        Path::new(checkout).join(format!("docs/programmes/{slug}"))
    }

    /// The path as the text quotes it for a shell.
    fn quoted(path: &Path) -> String {
        shell_quote(&path.display().to_string())
    }

    fn day(year: i32, month: u32, date: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, date).expect("a real day")
    }

    fn found(slug: &str, source: Source) -> Found {
        Found {
            repo: PathBuf::from("/work/repo"),
            checkout: PathBuf::from("/work/repo"),
            slug: slug.to_string(),
            source,
        }
    }

    fn row(programme: usize, slug: &str, id: &str, name: &str, due: Option<NaiveDate>) -> ReadyRow {
        ReadyRow {
            programme,
            slug: slug.to_string(),
            id: id.to_string(),
            name: name.to_string(),
            due,
            held: None,
        }
    }

    fn held_row(id: &str, session: &str, age_secs: u64) -> ReadyRow {
        let holder = if session.is_empty() {
            claim::Holder::Unreadable
        } else {
            claim::Holder::Session {
                id: session.to_string(),
                kind: claim::Kind::Work,
            }
        };
        ReadyRow {
            held: Some(HeldBy { holder, age_secs }),
            ..row(0, "prog", id, "Held name", None)
        }
    }

    fn ready_of(founds: Vec<Found>, rows: Vec<ReadyRow>) -> Ready {
        Ready {
            found: founds,
            rows,
            unreadable: Vec::new(),
        }
    }

    fn two_programmes() -> Vec<Found> {
        vec![
            found("alpha", Source::WorkingTree),
            found("beta", Source::Ref("origin/main".into())),
        ]
    }

    fn render_of(ready: &Ready, handoff: bool) -> String {
        render(
            Path::new("/work/repo"),
            ready,
            day(2026, 11, 17),
            |_| handoff,
            None,
        )
        .expect("something is ready")
    }

    #[test]
    fn nothing_ready_says_nothing() {
        let ready = ready_of(two_programmes(), Vec::new());
        assert_eq!(
            render(
                Path::new("/work/repo"),
                &ready,
                day(2026, 11, 17),
                |_| true,
                None
            ),
            None
        );
    }

    #[test]
    fn rows_print_their_marks_and_today_is_due_not_overdue() {
        let ready = ready_of(
            two_programmes(),
            vec![
                row(1, "beta", "t2", "Overdue one", Some(day(2026, 11, 1))),
                row(0, "alpha", "t1", "Due today", Some(day(2026, 11, 17))),
                row(0, "alpha", "t3", "Undated", None),
            ],
        );
        let text = render_of(&ready, false);
        assert!(
            text.starts_with("relais/programmes: 3 ready in /work/repo:\n"),
            "{text}"
        );
        assert!(
            text.contains("- beta t2: [overdue 2026-11-01] Overdue one\n"),
            "{text}"
        );
        assert!(
            text.contains("- alpha t1: [due 2026-11-17] Due today\n"),
            "{text}"
        );
        assert!(text.contains("- alpha t3: Undated\n"), "{text}");
    }

    #[test]
    fn a_name_past_the_cap_is_cut_with_an_ellipsis() {
        let long = "x".repeat(NAME_CAP + 40);
        let ready = ready_of(
            two_programmes(),
            vec![
                row(0, "alpha", "t1", &long, None),
                row(0, "alpha", "t2", &"y".repeat(NAME_CAP), None),
            ],
        );
        let text = render_of(&ready, false);
        assert!(
            text.contains(&format!("- alpha t1: {}…\n", "x".repeat(NAME_CAP))),
            "{text}"
        );
        assert!(
            text.contains(&format!("- alpha t2: {}\n", "y".repeat(NAME_CAP))),
            "{text}"
        );
    }

    #[test]
    fn past_eight_tickets_the_rest_is_counted_with_its_programmes() {
        let mut rows: Vec<ReadyRow> = (0..8)
            .map(|n| row(0, "alpha", &format!("t{n}"), "Shown", None))
            .collect();
        rows.push(row(0, "alpha", "t8", "Hidden", None));
        rows.push(row(1, "beta", "t9", "Hidden", None));
        let text = render_of(&ready_of(two_programmes(), rows), false);
        assert_eq!(text.matches("Shown\n").count(), 8, "{text}");
        assert!(!text.contains("Hidden\n"), "{text}");
        assert!(
            text.contains(&format!(
                "- … and 2 more ready (2 programmes: alpha, beta; relais programme frontier --no-fetch {} lists the first, 1 more)\n",
                quoted(&programme_dir("/work/repo", "alpha"))
            )),
            "{text}"
        );
        assert!(
            !text.contains("- beta:"),
            "a programme with no shown ticket has no locator: {text}"
        );
    }

    #[test]
    fn hidden_programmes_in_other_checkouts_name_the_first_hidden_one() {
        let mut founds = two_programmes();
        founds.push(found("gamma", Source::WorkingTree));
        founds[1].checkout = PathBuf::from("/work/repo-wt");
        founds[2].checkout = PathBuf::from("/work/other");
        let mut rows: Vec<ReadyRow> = (0..8)
            .map(|n| row(0, "alpha", &format!("t{n}"), "Shown", None))
            .collect();
        rows.push(row(2, "gamma", "t8", "Hidden", None));
        rows.push(row(1, "beta", "t9", "Hidden", None));
        rows.push(row(2, "gamma", "t10", "Hidden", None));
        let text = render_of(&ready_of(founds, rows), false);
        assert!(
            text.contains(&format!(
                "- … and 3 more ready (2 programmes: gamma, beta; relais programme frontier --no-fetch {} lists the first, 1 more)\n",
                quoted(&programme_dir("/work/other", "gamma"))
            )),
            "{text}"
        );
    }

    #[test]
    fn many_hidden_programmes_cap_their_names() {
        let slugs: Vec<String> = (0..=UNREAD_NAME_CAP).map(|n| format!("p{n}")).collect();
        let mut founds = vec![found("shown", Source::WorkingTree)];
        founds.extend(slugs.iter().map(|slug| found(slug, Source::WorkingTree)));
        let mut rows: Vec<ReadyRow> = (0..8)
            .map(|n| row(0, "shown", &format!("t{n}"), "Shown", None))
            .collect();
        rows.extend(
            slugs
                .iter()
                .enumerate()
                .map(|(n, slug)| row(n + 1, slug, "h", "Hidden", None)),
        );
        let text = render_of(&ready_of(founds, rows), false);
        let named = slugs[..UNREAD_NAME_CAP].join(", ");
        assert!(
            text.contains(&format!(
                "({} programmes: {named}, …; relais programme frontier --no-fetch {} lists the first, {} more)\n",
                slugs.len(),
                quoted(&programme_dir("/work/repo", "p0")),
                slugs.len() - 1
            )),
            "{text}"
        );
    }

    #[test]
    fn one_hidden_programme_is_singular_and_its_directory_is_quoted() {
        let mut founds = two_programmes();
        founds[0].checkout = PathBuf::from("/work/my repo");
        let mut rows: Vec<ReadyRow> = (0..8)
            .map(|n| row(1, "beta", &format!("t{n}"), "Shown", None))
            .collect();
        rows.push(row(0, "alpha", "t9", "Hidden", None));
        let text = render_of(&ready_of(founds, rows), false);
        assert!(
            text.contains(&format!(
                "- … and 1 more ready (1 programme; relais programme frontier --no-fetch {} lists them)\n",
                quoted(&programme_dir("/work/my repo", "alpha"))
            )),
            "{text}"
        );
    }

    #[test]
    fn exactly_eight_tickets_print_no_more_line() {
        let rows: Vec<ReadyRow> = (0..8)
            .map(|n| row(0, "alpha", &format!("t{n}"), "Shown", None))
            .collect();
        let text = render_of(&ready_of(two_programmes(), rows), false);
        assert!(!text.contains("more ready"), "{text}");
    }

    #[test]
    fn failing_loads_are_named_and_the_check_runs_on_the_first() {
        let mut ready = ready_of(two_programmes(), vec![row(0, "alpha", "t1", "Ready", None)]);
        ready.unreadable = vec![
            PathBuf::from("/work/repo/docs/programmes/broken"),
            PathBuf::from("/work/repo/docs/programmes/worse"),
        ];
        let text = render_of(&ready, false);
        assert!(
            text.starts_with(
                "relais/programmes: 1 ready in /work/repo (2 programmes could not be read: broken, worse; run relais programme check /work/repo/docs/programmes/broken for why on the first, 1 more):\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn the_unreadable_header_quotes_the_directory_to_check() {
        let mut ready = ready_of(two_programmes(), vec![row(0, "alpha", "t1", "Ready", None)]);
        ready.unreadable = vec![PathBuf::from("/work/my repo/docs/programmes/broken")];
        let text = render_of(&ready, false);
        assert!(
            text.contains(
                "(1 programme could not be read; run relais programme check '/work/my repo/docs/programmes/broken' for why)"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_checkout_programme_prints_its_directory_and_a_ref_programme_a_quoted_git_show() {
        let mut founds = two_programmes();
        founds[1].repo = PathBuf::from("/work/my repo");
        let ready = ready_of(
            founds,
            vec![
                row(0, "alpha", "t1", "A", None),
                row(1, "beta", "t2", "B", None),
            ],
        );
        let text = render_of(&ready, false);
        assert!(
            text.contains(&format!(
                "- alpha: {}\n",
                programme_dir("/work/repo", "alpha").display()
            )),
            "{text}"
        );
        assert!(
            text.contains(
                "- beta: git -C '/work/my repo' show origin/main:docs/programmes/beta/t2.md\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn handoff_pointers_follow_each_programmes_source_and_stop_at_three() {
        let founds: Vec<Found> = (0..5)
            .map(|n| {
                let source = if n == 1 {
                    Source::Ref("origin/main".into())
                } else {
                    Source::WorkingTree
                };
                found(&format!("p{n}"), source)
            })
            .collect();
        let rows: Vec<ReadyRow> = (0..5)
            .map(|n| row(n, &format!("p{n}"), "t1", "Ready", None))
            .collect();
        let text = render_of(&ready_of(founds, rows), true);
        assert_eq!(
            text.matches("says where the last session stopped").count(),
            3,
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "- p0: the ## Handoff in {}",
                programme_dir("/work/repo", "p0").join("map.md").display()
            )),
            "{text}"
        );
        assert!(
            text.contains(
                "- p1: run git -C /work/repo show origin/main:docs/programmes/p1/map.md; its ## Handoff says where the last session stopped (relais-handoff/1)\n"
            ),
            "{text}"
        );
        assert!(text.contains(HANDOFF_CONTRACT), "{text}");
        assert!(!text.contains("- p3: the ## Handoff"), "{text}");
    }

    #[test]
    fn a_programme_with_no_handoff_has_no_pointer() {
        let ready = ready_of(two_programmes(), vec![row(0, "alpha", "t1", "A", None)]);
        let text = render_of(&ready, false);
        assert!(!text.contains("handoff:"), "{text}");
    }

    #[test]
    fn gather_says_why_it_said_nothing() {
        let outside = crate::test_support::short_temp_dir("gather-outside");
        let none = gather(&outside).expect_err("no repository");
        assert_eq!(none.why, SkipReason::Error);
        let text = none.error.expect("text");
        assert!(
            text.starts_with("not a checkout: ")
                && text.ends_with(&format!("; no git repository under {}", outside.display())),
            "{text}"
        );
        let missing = gather(&outside.join("missing")).expect_err("a missing directory");
        assert_eq!(missing.why, SkipReason::Error);
        assert!(
            missing
                .error
                .as_deref()
                .is_some_and(|text| text.contains("missing")),
            "{missing:?}"
        );

        let repo = crate::test_support::short_temp_dir("gather-empty");
        let git = |args: &[&str]| {
            let out = crate::workspace::git_command(&repo)
                .args(args)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        assert_eq!(gather(&repo), Err(SkipReason::Nothing.into()));
    }

    fn reading(name: &str, rows: Vec<ReadyRow>, handoff_at: Option<i64>) -> RepoReading {
        reading_with(name, rows, handoff_at.map(Some))
    }

    /// `handoff`: `None` has no `## Handoff`, `Some(None)` has one whose map
    /// time could not be read.
    fn reading_with(name: &str, rows: Vec<ReadyRow>, handoff: Option<Option<i64>>) -> RepoReading {
        let repo = PathBuf::from(format!("/root/{name}"));
        let found = vec![Found {
            repo: repo.clone(),
            checkout: repo.clone(),
            slug: "prog".into(),
            source: Source::WorkingTree,
        }];
        RepoReading {
            repo,
            ready: ready_of(found, rows),
            handoffs: handoff.iter().map(|_| "prog".to_string()).collect(),
            latest_handoff: handoff.map(|at| LatestHandoff {
                slug: "prog".into(),
                at,
            }),
        }
    }

    fn rows_of(count: usize, due: Option<NaiveDate>) -> Vec<ReadyRow> {
        (0..count)
            .map(|n| row(0, "prog", &format!("t{n}"), "Name", due))
            .collect()
    }

    fn collected_of(read: Vec<RepoReading>, late: &[&str]) -> Collected<RepoReading> {
        Collected {
            read,
            late: late
                .iter()
                .map(|n| PathBuf::from(format!("/root/{n}")))
                .collect(),
            unreadable: Vec::new(),
        }
    }

    fn outside(collected: &Collected<RepoReading>) -> Result<String, SkipReason> {
        render_outside(Path::new("/root"), collected, day(2026, 11, 18)).map_err(|skip| skip.why)
    }

    #[test]
    fn collect_within_renders_a_partial_result_naming_the_late_repository() {
        // The slow reader blocks on a channel nothing sends to until the
        // test ends, so only the cutoff decides that it is late.
        let (hold, blocked) = mpsc::channel::<()>();
        let blocked = std::sync::Mutex::new(blocked);
        let read = move |repo: &Path| {
            let name = repo
                .file_name()
                .expect("name")
                .to_string_lossy()
                .to_string();
            if name == "slow" {
                assert!(
                    blocked.lock().expect("lock").recv().is_err(),
                    "nothing is sent"
                );
            }
            Ok(reading(&name, rows_of(1, None), None))
        };
        let repos = vec![PathBuf::from("/root/fast"), PathBuf::from("/root/slow")];
        let collected = collect_within(repos, read, Instant::now() + Duration::from_secs(1));
        drop(hold);
        assert_eq!(collected.read.len(), 1);
        assert_eq!(collected.late, [PathBuf::from("/root/slow")]);
        assert!(collected.unreadable.is_empty());
        let text = outside(&collected).expect("partial result");
        assert!(
            text.contains(
                "(1 repository not read in time: slow; relais programme list --no-fetch --root /root lists it)\n"
            ),
            "{text}"
        );
        assert!(text.contains("- fast: 1 ready; cd /root/fast\n"), "{text}");
    }

    #[test]
    fn a_panicking_or_failing_reader_is_unreadable_with_its_reason() {
        let read = |repo: &Path| {
            if repo.ends_with("bad") {
                panic!("a reader that dies");
            }
            if repo.ends_with("broken") {
                return Err("no programmes".to_string());
            }
            Ok(1)
        };
        let repos = vec![
            PathBuf::from("/root/good"),
            PathBuf::from("/root/bad"),
            PathBuf::from("/root/broken"),
        ];
        let collected = collect_within(repos, read, Instant::now() + Duration::from_secs(5));
        assert_eq!(collected.read, [1]);
        assert!(collected.late.is_empty());
        let unreadable: Vec<(&Path, &str)> = collected
            .unreadable
            .iter()
            .map(|(repo, reason)| (repo.as_path(), reason.as_str()))
            .collect();
        assert_eq!(
            unreadable,
            [
                (Path::new("/root/bad"), "its reader panicked"),
                (Path::new("/root/broken"), "no programmes"),
            ]
        );
    }

    #[test]
    fn unreadable_repositories_get_their_own_line_with_the_command_to_run() {
        let mut collected = collected_of(vec![reading("a", rows_of(1, None), None)], &["l1"]);
        collected.unreadable = vec![(PathBuf::from("/root/my c"), "why".into())];
        let text = outside(&collected).expect("text");
        assert!(
            text.contains("(1 repository could not be read: my c (first: why); relais programme list --no-fetch --root /root says why)\n"),
            "{text}"
        );
        assert!(
            text.contains("(1 repository not read in time: l1; relais programme list --no-fetch --root /root lists it)\n"),
            "{text}"
        );
        collected
            .unreadable
            .push((PathBuf::from("/root/d"), "why".into()));
        let text = outside(&collected).expect("text");
        assert!(
            text.contains("(2 repositories could not be read: my c, d (first: why); relais programme list --no-fetch --root /root says why)\n"),
            "{text}"
        );
    }

    #[test]
    fn a_handoff_with_a_known_time_beats_one_with_none_whatever_the_order() {
        for untimed_first in [true, false] {
            let untimed = reading_with("untimed", rows_of(1, None), Some(None));
            let timed = reading("timed", rows_of(1, None), Some(5));
            let read = if untimed_first {
                vec![untimed, timed]
            } else {
                vec![timed, untimed]
            };
            let text = outside(&collected_of(read, &[])).expect("text");
            assert!(text.contains("expanded: timed ("), "{text}");
        }
    }

    #[test]
    fn nothing_read_in_time_is_a_deadline_skip() {
        let collected = collected_of(Vec::new(), &["a"]);
        assert_eq!(outside(&collected), Err(SkipReason::Deadline));
    }

    #[test]
    fn every_repository_unreadable_is_an_error_naming_each_reason() {
        let collected: Collected<RepoReading> = Collected {
            read: Vec::new(),
            late: Vec::new(),
            unreadable: vec![
                (PathBuf::from("/root/a"), "git broke".to_string()),
                (PathBuf::from("/root/b"), "its reader panicked".to_string()),
            ],
        };
        let skip = render_outside(Path::new("/root"), &collected, day(2026, 11, 18))
            .expect_err("nothing was read");
        assert_eq!(
            skip,
            Skip::error("/root/a: git broke; /root/b: its reader panicked")
        );
    }

    #[test]
    fn a_deadline_with_unreadable_repositories_keeps_their_reasons() {
        let collected: Collected<RepoReading> = Collected {
            read: Vec::new(),
            late: vec![PathBuf::from("/root/slow")],
            unreadable: vec![(PathBuf::from("/root/a"), "git broke".to_string())],
        };
        let skip = render_outside(Path::new("/root"), &collected, day(2026, 11, 18))
            .expect_err("nothing was read");
        assert_eq!(skip.why, SkipReason::Deadline);
        assert_eq!(skip.error.as_deref(), Some("/root/a: git broke"));
    }

    #[test]
    fn nothing_ready_and_no_handoff_is_nothing() {
        let collected = collected_of(vec![reading("a", Vec::new(), None)], &[]);
        assert_eq!(outside(&collected), Err(SkipReason::Nothing));
    }

    #[test]
    fn summaries_order_by_soonest_due_then_most_ready_then_name() {
        let soon = Some(day(2026, 11, 20));
        let later = Some(day(2026, 12, 1));
        let collected = collected_of(
            vec![
                reading("zeta", rows_of(5, None), None),
                reading("beta", rows_of(2, None), None),
                reading("alpha", rows_of(2, None), None),
                reading("late", rows_of(1, later), None),
                reading("soon", rows_of(1, soon), None),
            ],
            &[],
        );
        let text = outside(&collected).expect("text");
        let summaries = text.split("expanded:").next().expect("summaries");
        let names: Vec<&str> = summaries
            .lines()
            .filter_map(|line| line.strip_prefix("- "))
            .filter_map(|line| line.split(':').next())
            .collect();
        assert_eq!(names, ["soon", "late", "zeta", "alpha", "beta"], "{text}");
        assert!(
            text.starts_with(
                "relais/programmes: 11 ready in 5 repositories under /root (not a repository):\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("- soon: 1 ready, 1 due (first 2026-11-20); cd /root/soon\n"),
            "{text}"
        );
    }

    #[test]
    fn summary_lines_are_capped_and_the_rest_counted() {
        let read: Vec<RepoReading> = (0..TICKET_LINE_CAP + 3)
            .map(|n| reading(&format!("r{n:02}"), rows_of(1, None), None))
            .collect();
        let text = outside(&collected_of(read, &[])).expect("text");
        assert_eq!(
            text.matches("; cd /root/").count(),
            TICKET_LINE_CAP,
            "{text}"
        );
        assert!(
            text.contains(
                "- … and 3 more repositories (relais programme list --no-fetch --root /root lists them)\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn one_more_summary_is_singular() {
        let read: Vec<RepoReading> = (0..TICKET_LINE_CAP + 1)
            .map(|n| reading(&format!("r{n:02}"), rows_of(1, None), None))
            .collect();
        let text = outside(&collected_of(read, &[])).expect("text");
        assert!(text.contains("- … and 1 more repository (relais"), "{text}");
    }

    #[test]
    fn the_latest_handoff_wins_the_expanded_block_and_is_labelled() {
        let collected = collected_of(
            vec![
                reading("old", rows_of(1, None), Some(100)),
                reading("new", rows_of(2, None), Some(200)),
            ],
            &[],
        );
        let text = outside(&collected).expect("text");
        assert!(
            text.contains("expanded: new (latest ## Handoff among those read)\n"),
            "{text}"
        );
        assert!(
            text.contains("relais/programmes: 2 ready in /root/new:\n"),
            "{text}"
        );
        assert!(
            !text.contains("relais/programmes: 1 ready in /root/old"),
            "{text}"
        );
    }

    #[test]
    fn with_no_handoff_the_first_summary_is_expanded_and_says_so() {
        let collected = collected_of(
            vec![
                reading("a", rows_of(1, None), None),
                reading("b", rows_of(3, None), None),
            ],
            &[],
        );
        let text = outside(&collected).expect("text");
        assert!(
            text.contains("expanded: b (no repository has a ## Handoff)\n"),
            "{text}"
        );
        assert!(!text.contains("handoff:"), "{text}");
    }

    #[test]
    fn a_winning_handoff_with_no_shown_ticket_is_still_pointed_at() {
        let mut winner = reading("win", Vec::new(), Some(300));
        winner.ready.found.push(Found {
            repo: PathBuf::from("/root/win"),
            checkout: PathBuf::from("/root/win"),
            slug: "other".into(),
            source: Source::WorkingTree,
        });
        // Eight tickets of "other" fill the cap; the winning "prog" has none shown.
        winner.ready.rows = (0..TICKET_LINE_CAP)
            .map(|n| row(1, "other", &format!("t{n}"), "Name", None))
            .collect();
        let text = outside(&collected_of(vec![winner], &[])).expect("text");
        assert!(
            text.contains(&format!(
                "handoff:\n- prog: the ## Handoff in {}",
                programme_dir("/root/win", "prog").join("map.md").display()
            )),
            "{text}"
        );
    }

    #[test]
    fn a_winning_repository_with_no_ready_ticket_prints_its_block_alone() {
        let collected = collected_of(vec![reading("quiet", Vec::new(), Some(10))], &[]);
        let text = outside(&collected).expect("text");
        assert!(!text.contains("(not a repository)"), "{text}");
        assert!(
            text.starts_with("expanded: quiet (latest ## Handoff among those read)\nrelais/programmes: 0 ready in /root/quiet:\n"),
            "{text}"
        );
        assert!(
            text.contains("handoff:\n- prog: the ## Handoff in "),
            "{text}"
        );
        assert!(!text.contains("read a ticket from:"), "{text}");
    }

    #[test]
    fn many_unread_repositories_are_listed_up_to_a_cap() {
        let collected = collected_of(
            vec![reading("a", rows_of(1, None), None)],
            &["u1", "u2", "u3", "u4", "u5", "u6"],
        );
        let text = outside(&collected).expect("text");
        assert!(
            text.contains("(6 repositories not read in time: u1, u2, u3, u4, u5, …; relais programme list --no-fetch --root /root lists them)\n"),
            "{text}"
        );
    }

    #[test]
    fn control_characters_in_a_repository_name_are_replaced() {
        let collected = collected_of(vec![reading("a\nb\u{1b}[31m", rows_of(1, None), None)], &[]);
        let text = outside(&collected).expect("text");
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(text.contains("- a?b?[31m: 1 ready"), "{text:?}");
        assert_eq!(
            text.split("expanded:")
                .next()
                .expect("summaries")
                .lines()
                .filter(|l| l.starts_with("- "))
                .count(),
            1,
            "{text:?}"
        );
    }

    #[test]
    fn a_pin_changes_nothing_when_absent_and_leads_the_pointers_when_given() {
        let founds: Vec<Found> = (0..2)
            .map(|n| found(&format!("p{n}"), Source::WorkingTree))
            .collect();
        let rows = vec![row(0, "p0", "t1", "A", None)];
        let ready = ready_of(founds, rows);
        let plain = render(
            Path::new("/work/repo"),
            &ready,
            day(2026, 11, 17),
            |_| true,
            None,
        )
        .expect("text");
        assert_eq!(
            plain,
            format!(
                "relais/programmes: 1 ready in /work/repo:\n- p0 t1: A\nread a ticket from:\n- p0: {}\nhandoff:\n- p0: the ## Handoff in {} says where the last session stopped (relais-handoff/1)\n",
                programme_dir("/work/repo", "p0").display(),
                programme_dir("/work/repo", "p0").join("map.md").display()
            )
        );
        let pinned = render(
            Path::new("/work/repo"),
            &ready,
            day(2026, 11, 17),
            |_| true,
            Some(&ready.found[1]),
        )
        .expect("text");
        let first_pointer = pinned.split("handoff:\n").nth(1).expect("pointers");
        assert!(first_pointer.starts_with("- p1: "), "{pinned}");
        assert!(first_pointer.contains("- p0: "), "{pinned}");
    }

    #[test]
    fn a_held_row_carries_its_mark_after_the_cut_name_and_sorts_after_unheld_rows() {
        let long = "x".repeat(NAME_CAP + 5);
        let mut held = held_row("h1", "0123456789abcdef", 7_300);
        held.name = long.clone();
        let rows = vec![
            held,
            row(0, "prog", "u1", "Open one", None),
            held_row("h2", "", 90),
        ];
        let ready = ready_of(vec![found("prog", Source::WorkingTree)], rows);
        let text = render_of(&ready, false);
        assert!(
            text.starts_with("relais/programmes: 1 ready, 2 held in /work/repo:\n"),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "- prog h1: {}… (held by 01234567, 2h)\n",
                "x".repeat(NAME_CAP)
            )),
            "the mark follows the cut name: {text}"
        );
        assert!(
            text.contains("- prog h2: Held name (held by unreadable, 1m)\n"),
            "{text}"
        );
        let order: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_prefix("- prog "))
            .filter_map(|line| line.split(':').next())
            .collect();
        assert_eq!(&order[..3], ["u1", "h1", "h2"], "{text}");
    }

    #[test]
    fn held_rows_fall_past_the_cap_before_unheld_ones_do() {
        let mut rows: Vec<ReadyRow> = (0..3).map(|n| held_row(&format!("h{n}"), "s", 0)).collect();
        rows.extend((0..TICKET_LINE_CAP).map(|n| row(0, "prog", &format!("u{n}"), "Open", None)));
        let ready = ready_of(vec![found("prog", Source::WorkingTree)], rows);
        let text = render_of(&ready, false);
        assert!(text.contains("- prog u7: Open\n"), "{text}");
        assert!(
            !text.contains("- prog h0:"),
            "held rows are the hidden ones: {text}"
        );
        assert!(text.contains("- … and 3 more ready"), "{text}");
    }

    #[test]
    fn at_a_projects_root_the_repository_with_the_most_unheld_ready_is_expanded() {
        // `busy` has the latest Handoff and the most rows, but all held but one.
        let mut busy_rows = rows_of(1, None);
        busy_rows.extend((0..4).map(|n| held_row(&format!("h{n}"), "s1", 0)));
        let collected = collected_of(
            vec![
                reading("busy", busy_rows, Some(900)),
                reading("free", rows_of(3, None), Some(100)),
                reading("also_free", rows_of(3, None), Some(500)),
            ],
            &[],
        );
        let text = outside(&collected).expect("text");
        assert!(
            text.contains("expanded: also_free (latest ## Handoff"),
            "most unheld first, then the latest Handoff: {text}"
        );
        assert!(
            text.contains("- busy: 1 ready, 4 held; cd /root/busy\n"),
            "{text}"
        );
        assert!(
            text.starts_with(
                "relais/programmes: 7 ready, 4 held in 3 repositories under /root (not a repository):\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn rendering_held_tickets_writes_no_claim_file() {
        let state = crate::test_support::short_temp_dir("resume-readonly");
        let before = std::fs::read_dir(&state).expect("state dir").count();
        let ready = ready_of(
            vec![found("prog", Source::WorkingTree)],
            vec![held_row("h", "s1", 0)],
        );
        render_of(&ready, true);
        assert_eq!(
            std::fs::read_dir(&state).expect("state dir").count(),
            before
        );
    }

    #[test]
    fn a_non_repository_root_with_no_child_repository_is_an_error() {
        let root = crate::test_support::short_temp_dir("gather-no-children");
        std::fs::create_dir_all(root.join("plain")).expect("plain directory");
        assert_eq!(
            gather(&root).map_err(|skip| skip.why),
            Err(SkipReason::Error)
        );
    }

    #[test]
    fn a_word_is_quoted_only_when_a_shell_would_read_it_specially() {
        assert_eq!(
            shell_quote("origin/main:docs/a.md"),
            "origin/main:docs/a.md"
        );
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
