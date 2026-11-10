//! `relais programme list|frontier|show|check` (SPEC §31): queries over
//! programme maps. They open no ledger, no coordinator and no machine
//! config, and never write a map; the one thing they write is the
//! remote-tracking refs a `git fetch` of origin updates. They read the
//! programme's files, or origin's default branch through git, and the
//! branch names git lists in the map's repository.

use std::io::Write;
use std::path::{Path, PathBuf};

use relais::programme::analysis::{self, age_secs, Counts, Standing};
use relais::programme::load::{self, main_checkout, Found, Programme, ProgrammeError, Source};
use relais::programme::model::Ticket;
use serde_json::{json, Value};

use super::outcome::{CliError, CliOutcome};

/// How a command prints what it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Text,
    Json,
}

impl Format {
    pub(crate) fn from_json_flag(json: bool) -> Self {
        if json {
            Self::Json
        } else {
            Self::Text
        }
    }
}

/// Whether a command fetches origin before reading it (SPEC §31).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fetch {
    /// `git fetch --prune origin` first, bounded.
    Origin,
    /// Read origin as the last fetch left it (`--no-fetch`).
    LastFetch,
}

impl Fetch {
    pub(crate) fn from_no_fetch_flag(no_fetch: bool) -> Self {
        if no_fetch {
            Self::LastFetch
        } else {
            Self::Origin
        }
    }
}

/// Which tickets `frontier` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Listing {
    ReadyOnly,
    WithInFlight,
}

impl Listing {
    pub(crate) fn from_all_flag(all: bool) -> Self {
        if all {
            Self::WithInFlight
        } else {
            Self::ReadyOnly
        }
    }
}

fn programme_error(error: ProgrammeError) -> CliError {
    match error {
        ProgrammeError::Read { path, cause } => CliError::Read {
            what: "the programme file",
            path,
            cause,
        },
        ProgrammeError::Layout { .. } => CliError::Usage {
            detail: error.to_string(),
        },
        ProgrammeError::Git { .. } => CliError::Operational {
            operation: "programme",
            cause: Box::new(error),
        },
    }
}

/// Writes the answer to stdout. A reader that went away early (`| head`)
/// is the normal end of a pipeline, not a failure.
fn print_stdout(text: &str) -> Result<(), CliError> {
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other.map_err(|cause| CliError::Operational {
            operation: "programme: writing stdout",
            cause: Box::new(cause),
        }),
    }
}

/// The columns of the terminal on stdout, when stdout is one and the shell
/// exported `COLUMNS`; otherwise every name stays whole. (Platform calls
/// belong to `procs`, and a width probe would be one more unbounded wait.)
fn terminal_width() -> Option<usize> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return None;
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|text| text.trim().parse::<usize>().ok())
        .filter(|columns| *columns > 0)
}

/// The columns `text` occupies from column 0, tabs advancing to the next
/// multiple of 8.
fn display_width(text: &str) -> usize {
    text.chars().fold(0, |column, c| {
        if c == '\t' {
            (column / 8 + 1) * 8
        } else {
            column + 1
        }
    })
}

/// `name`, cut with `…` only when it would push a line that already
/// occupies `prefix_width` columns past `width`.
fn fit_name(name: &str, prefix_width: usize, width: Option<usize>) -> String {
    let Some(width) = width else {
        return name.to_string();
    };
    let room = width.saturating_sub(prefix_width);
    if name.chars().count() <= room {
        return name.to_string();
    }
    let kept: String = name.chars().take(room.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// A branch tip as `frontier --all` and `show` print it.
struct Tip<'a> {
    branch: &'a str,
    age_secs: u64,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0) // only a clock before 1970 lands here; ages then clamp to 0 rather than fail a read-only listing
}

fn tip_of(standing: &Standing, now_unix: i64) -> Option<Tip<'_>> {
    match standing {
        Standing::InFlight {
            branch,
            last_commit_unix,
        } => Some(Tip {
            branch,
            age_secs: age_secs(now_unix, *last_commit_unix),
        }),
        Standing::Frontier | Standing::Blocked(_) | Standing::Closed | Standing::OutOfScope => None,
    }
}

/// `5m`, `3h` or `2d`: the largest whole unit.
fn format_age(secs: u64) -> String {
    match secs {
        0..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

fn ticket_json(programme: &Programme, ticket: &Ticket, tip: Option<&Tip<'_>>) -> Value {
    json!({
        "id": ticket.id,
        "type": ticket.kind.as_str(),
        "mode": ticket.mode.as_str(),
        "repos": programme.repos_of(ticket),
        "name": ticket.name,
        "branch": tip.map(|t| t.branch),
        "branch_age_secs": tip.map(|t| t.age_secs),
    })
}

fn counts_json(counts: &Counts) -> Value {
    json!({
        "open": counts.open,
        "frontier": counts.frontier,
        "blocked": counts.blocked,
        "in_flight": counts.in_flight,
        "closed": counts.closed,
        "out_of_scope": counts.out_of_scope,
    })
}

pub(crate) fn frontier_command(
    dir: &Path,
    listing: Listing,
    format: Format,
    fetch: Fetch,
) -> Result<CliOutcome, CliError> {
    let found = resolve_dir(dir, fetch)?;
    if let (Source::Ref(rev), true) = (&found.source, stderr_is_terminal()) {
        eprintln!("relais: reading {} from {rev}", dir.display());
    }
    let programme = found.load().map_err(programme_error)?;
    let standings = programme.standings().map_err(programme_error)?;
    let now = now_unix();
    let shown: Vec<(&Ticket, Option<Tip<'_>>)> = programme
        .tickets
        .values()
        .filter_map(|ticket| match (&standings[&ticket.id], listing) {
            (Standing::Frontier, _) => Some((ticket, None)),
            (standing @ Standing::InFlight { .. }, Listing::WithInFlight) => {
                Some((ticket, tip_of(standing, now)))
            }
            (Standing::InFlight { .. }, Listing::ReadyOnly)
            | (
                Standing::Blocked(_) | Standing::Closed | Standing::OutOfScope,
                Listing::ReadyOnly | Listing::WithInFlight,
            ) => None,
        })
        .collect();
    if shown.is_empty() {
        let counts = analysis::counts(&standings);
        eprintln!(
            "no open ticket is ready: {} blocked, {} in flight",
            counts.blocked, counts.in_flight
        );
        eprintln!("next: relais programme show {}", dir.display());
    }
    let out = match format {
        Format::Json => {
            let rows: Vec<Value> = shown
                .iter()
                .map(|(ticket, tip)| ticket_json(&programme, ticket, tip.as_ref()))
                .collect();
            format!("{}\n", Value::Array(rows))
        }
        Format::Text => {
            let width = terminal_width();
            let mut text = String::new();
            for (ticket, tip) in &shown {
                let prefix = format!(
                    "{}\t{}/{}\t{}\t",
                    ticket.id,
                    ticket.kind.as_str(),
                    ticket.mode.as_str(),
                    programme.repos_of(ticket).join(",")
                );
                // Under --all every line has five fields: a ready ticket's
                // fifth is empty, so `cut` and `awk` see one shape.
                let label = match (tip, listing) {
                    (Some(t), _) => {
                        format!("\tin-flight {} {}", t.branch, format_age(t.age_secs))
                    }
                    (None, Listing::WithInFlight) => "\t".to_string(),
                    (None, Listing::ReadyOnly) => String::new(),
                };
                let name = fit_name(
                    &ticket.name,
                    display_width(&prefix) + display_width(&label),
                    width,
                );
                text.push_str(&format!("{prefix}{name}{label}\n"));
            }
            text
        }
    };
    print_stdout(&out)?;
    Ok(CliOutcome::Accepted)
}

pub(crate) fn show_command(
    dir: &Path,
    format: Format,
    fetch: Fetch,
) -> Result<CliOutcome, CliError> {
    let found = resolve_dir(dir, fetch)?;
    let programme = found.load().map_err(programme_error)?;
    let standings = programme.standings().map_err(programme_error)?;
    let counts = analysis::counts(&standings);
    let blocked: Vec<(&Ticket, &Vec<String>)> = programme
        .tickets
        .values()
        .filter_map(|t| match &standings[&t.id] {
            Standing::Blocked(by) => Some((t, by)),
            Standing::Frontier
            | Standing::InFlight { .. }
            | Standing::Closed
            | Standing::OutOfScope => None,
        })
        .collect();
    let now = now_unix();
    let in_flight: Vec<(&Ticket, Tip<'_>)> = programme
        .tickets
        .values()
        .filter_map(|t| tip_of(&standings[&t.id], now).map(|tip| (t, tip)))
        .collect();
    let out = match format {
        Format::Json => format!(
            "{}\n",
            json!({
                "destination": programme.map.destination,
                "source": found.source.label(),
                "counts": counts_json(&counts),
                "blocked": blocked.iter().map(|(t, by)| json!({
                    "id": t.id, "name": t.name, "blocked_by": by,
                })).collect::<Vec<_>>(),
                "in_flight": in_flight.iter().map(|(t, tip)| json!({
                    "id": t.id, "name": t.name, "branch": tip.branch,
                    "branch_age_secs": tip.age_secs,
                })).collect::<Vec<_>>(),
            })
        ),
        Format::Text => {
            let mut text = format!(
                "Destination\n{}\n\nRead from {}\n\n",
                programme.map.destination,
                found.source.label()
            );
            text.push_str(&format!(
                "open {} (frontier {}, blocked {}, in flight {}), closed {}, out of scope {}\n",
                counts.open,
                counts.frontier,
                counts.blocked,
                counts.in_flight,
                counts.closed,
                counts.out_of_scope
            ));
            if !blocked.is_empty() {
                text.push_str("\nBlocked\n");
                for (ticket, by) in &blocked {
                    text.push_str(&format!(
                        "  {} ({}): blocked by {}\n",
                        ticket.name,
                        ticket.id,
                        by.join(", ")
                    ));
                }
            }
            if !in_flight.is_empty() {
                text.push_str("\nIn flight\n");
                for (ticket, tip) in &in_flight {
                    text.push_str(&format!(
                        "  {} ({}): {} {}\n",
                        ticket.name,
                        ticket.id,
                        tip.branch,
                        format_age(tip.age_secs)
                    ));
                }
            }
            text
        }
    };
    print_stdout(&out)?;
    Ok(CliOutcome::Accepted)
}

pub(crate) fn check_command(
    dir: &Path,
    format: Format,
    fetch: Fetch,
) -> Result<CliOutcome, CliError> {
    let found = resolve_dir(dir, fetch)?;
    let programme = found.load().map_err(programme_error)?;
    let tag = ref_tag(&found.source);
    let problems = programme.problems();
    for problem in &problems {
        eprintln!(
            "{}:{}: {tag}{}",
            dir.join(&problem.file).display(),
            problem.line,
            problem.message
        );
    }
    if !problems.is_empty() {
        eprintln!("{} problems", problems.len());
    }
    let warnings = analysis::warnings(&programme.tickets);
    for warning in &warnings {
        eprintln!(
            "{}:{}: {tag}warning: {}",
            dir.join(&warning.file).display(),
            warning.line,
            warning.message
        );
    }
    if format == Format::Json {
        let rows = |found: &[relais::programme::model::Problem]| -> Vec<Value> {
            found
                .iter()
                .map(|p| {
                    json!({
                        "path": dir.join(&p.file).display().to_string(),
                        "line": p.line,
                        "message": p.message,
                    })
                })
                .collect()
        };
        print_stdout(&format!(
            "{}\n",
            json!({
                "problems": rows(&problems),
                "warnings": rows(&warnings),
                "source": found.source.label(),
            })
        ))?;
    }
    Ok(if problems.is_empty() {
        CliOutcome::Accepted
    } else {
        CliOutcome::InvalidInput
    })
}

/// The directory `list` searches when `--root` is not given: the parent of
/// the current repository's main checkout, else the working directory itself.
fn default_root() -> Result<PathBuf, CliError> {
    let cwd = std::env::current_dir().map_err(CliError::Cwd)?;
    Ok(main_checkout(&cwd)
        .ok()
        .and_then(|repo| repo.parent().map(Path::to_path_buf))
        .unwrap_or(cwd))
}

/// How many repositories `list` fetches at once.
const FETCH_PARALLEL: usize = 8;

/// Whether stderr is a terminal: progress lines go only there.
fn stderr_is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

/// `f` over `items` on up to [`FETCH_PARALLEL`] threads, the results in
/// the items' order. Every git call costs a process, so the repositories
/// under a root are read side by side, as they are fetched.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<(usize, R)>> = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..FETCH_PARALLEL.min(items.len()) {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(item) = items.get(index) else {
                    break;
                };
                let mapped = f(item);
                results
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((index, mapped));
            });
        }
    });
    let mut results = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

/// Fetches origin in each of `repos` that has one, up to
/// [`FETCH_PARALLEL`] at once, and names each failure on stderr, in
/// repository order, before anything else is printed (SPEC §31). A
/// failure leaves the last fetch to be read and never changes the exit code.
fn fetch_all(repos: &[PathBuf]) {
    let with_origin: Vec<&PathBuf> = repos.iter().filter(|r| load::has_origin(r)).collect();
    if with_origin.is_empty() {
        return;
    }
    if stderr_is_terminal() {
        let what = match with_origin.as_slice() {
            [one] => one.file_name().map_or_else(
                || one.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            many => format!("{} repositories", many.len()),
        };
        eprintln!("relais: fetching {what} (--no-fetch skips)");
    }
    let failures: Vec<(usize, load::FetchFailure)> = par_map(&with_origin, |repo| {
        load::fetch_origin(repo, load::FETCH_BACKSTOP)
    })
    .into_iter()
    .enumerate()
    .filter_map(|(index, fetched)| fetched.err().map(|failure| (index, failure)))
    .collect();
    for (index, failure) in &failures {
        eprintln!(
            "relais: {}: fetch failed ({failure}); reading the last fetch",
            with_origin[*index].display()
        );
    }
    if !failures.is_empty() {
        eprintln!("relais: run with --no-fetch to skip fetching");
    }
}

/// Locates `dir`, fetches its repository unless told not to, resolves
/// where to read it from (SPEC §31) and names on stderr what the
/// resolution set aside or skipped.
fn resolve_dir(dir: &Path, fetch: Fetch) -> Result<Found, CliError> {
    let located = load::locate(dir).map_err(programme_error)?;
    if fetch == Fetch::Origin {
        fetch_all(std::slice::from_ref(&located.repo));
    }
    let resolved = load::resolve(&located).map_err(programme_error)?;
    for note in &resolved.notes {
        eprintln!("relais: {note}");
    }
    Ok(resolved.found)
}

/// `(<ref>) ` before a message whose line numbers are the ref's, not the
/// files on disk; nothing when the files were read.
fn ref_tag(source: &Source) -> String {
    match source {
        Source::WorkingTree => String::new(),
        Source::Ref(rev) => format!("({rev}) "),
    }
}

pub(crate) fn list_command(
    root: Option<&Path>,
    format: Format,
    fetch: Fetch,
) -> Result<CliOutcome, CliError> {
    let root = match root {
        Some(root) => root.to_path_buf(),
        None => default_root()?,
    };
    // A repository that holds no programme anywhere, as of its last fetch,
    // is neither fetched nor read: most repositories under a root have none.
    let all = load::repositories(&root).map_err(programme_error)?;
    let holds = par_map(&all, |repo| load::may_hold_programmes(repo));
    let repos: Vec<PathBuf> = all
        .into_iter()
        .zip(holds)
        .filter_map(|(repo, holds)| holds.then_some(repo))
        .collect();
    if fetch == Fetch::Origin {
        fetch_all(&repos);
    }
    let mut rows: Vec<(PathBuf, String, Counts, String)> = Vec::new();
    let mut outcome = CliOutcome::Accepted;
    let mut found_all = Vec::new();
    for (repo, found) in repos
        .iter()
        .zip(par_map(&repos, |repo| load::programmes_of(repo)))
    {
        match found {
            Ok(listed) => {
                for note in &listed.notes {
                    eprintln!("relais: {note}");
                }
                found_all.extend(listed.found);
            }
            Err(error) => {
                eprintln!("relais: {}: {error}", repo.display());
                outcome = CliOutcome::OperationalFailure;
            }
        }
    }
    let loaded_all = par_map(&found_all, |found| {
        found
            .load()
            .and_then(|programme| programme.standings().map(|s| (programme, s)))
    });
    for (found, loaded) in found_all.iter().zip(loaded_all) {
        let dir = found.dir();
        match loaded {
            Ok((programme, standings)) => {
                let destination = programme
                    .map
                    .destination
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                rows.push((
                    dir.join(relais::programme::model::MAP_FILE),
                    destination,
                    analysis::counts(&standings),
                    found.source.label().to_string(),
                ));
            }
            Err(error) => {
                eprintln!("relais: {}: {error}", dir.display());
                outcome = CliOutcome::OperationalFailure;
            }
        }
    }
    let out = match format {
        Format::Json => {
            let items: Vec<Value> = rows
                .iter()
                .map(|(path, destination, counts, source)| {
                    json!({
                        "path": path.display().to_string(),
                        "destination": destination,
                        "open": counts.open,
                        "frontier": counts.frontier,
                        "closed": counts.closed,
                        "source": source,
                    })
                })
                .collect();
            format!("{}\n", Value::Array(items))
        }
        Format::Text => rows
            .iter()
            .map(|(path, destination, counts, source)| {
                format!(
                    "{}\t{destination}\topen {} frontier {} closed {}\t{source}\n",
                    path.display(),
                    counts.open,
                    counts.frontier,
                    counts.closed
                )
            })
            .collect(),
    };
    print_stdout(&out)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_cut_only_when_the_terminal_is_narrower_than_the_line() {
        assert_eq!(fit_name("A long name", 10, None), "A long name");
        assert_eq!(fit_name("A long name", 10, Some(21)), "A long name");
        assert_eq!(fit_name("A long name", 10, Some(15)), "A lo…");
        assert_eq!(fit_name("A long name", 30, Some(15)), "…");
    }

    #[test]
    fn an_age_is_written_in_its_largest_whole_unit() {
        assert_eq!(format_age(0), "0m");
        assert_eq!(format_age(300), "5m");
        assert_eq!(format_age(10_800), "3h");
        assert_eq!(format_age(2 * 86_400 + 5), "2d");
    }

    #[test]
    fn tabs_advance_to_the_next_multiple_of_eight() {
        assert_eq!(display_width("A\tB"), 9);
        assert_eq!(display_width("\t\t"), 16);
    }
}
