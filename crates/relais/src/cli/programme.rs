//! `relais programme list|frontier|show|check` (SPEC §31): queries over
//! programme maps. They open no ledger, no coordinator and no machine
//! config, and never write a map; the one thing they write is the
//! remote-tracking refs a `git fetch` of origin updates. They read the
//! programme's files, or origin's default branch through git, and the
//! branch names git lists in the map's repository.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use relais::programme::analysis::{self, age_secs, Counts, Holder, Standing};
use relais::programme::claim;
use relais::programme::frontier::{
    due_mark, format_age, is_overdue, is_ready, order_frontier, short_session,
};
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

/// What holds a ticket, and for how long, as `frontier --all` and `show`
/// print it.
struct Tip<'a> {
    holder: &'a Holder,
    age_secs: u64,
}

impl Tip<'_> {
    /// `in-flight <branch> <age>` or `claimed <s8> <age>`.
    fn label(&self) -> String {
        let age = format_age(self.age_secs);
        match self.holder {
            Holder::Branch { branch, .. } => format!("in-flight {branch} {age}"),
            Holder::Claim(hold) => format!("claimed {} {age}", short_session(&hold.session)),
        }
    }

    /// `<branch> <age>` or `claimed <s8> <age>`, as `show` lists it.
    fn holder_text(&self) -> String {
        let age = format_age(self.age_secs);
        match self.holder {
            Holder::Branch { branch, .. } => format!("{branch} {age}"),
            Holder::Claim(hold) => format!("claimed {} {age}", short_session(&hold.session)),
        }
    }

    /// The holder as `held_by` in JSON: `null` when nothing holds the ticket.
    fn json(tip: Option<&Self>) -> Value {
        match tip {
            None => Value::Null,
            Some(Tip {
                holder: Holder::Branch { branch, .. },
                age_secs,
            }) => json!({"holder": "branch", "branch": branch, "age_secs": age_secs}),
            Some(Tip {
                holder: Holder::Claim(hold),
                age_secs,
            }) => json!({
                "holder": "claim",
                "session": hold.session,
                "kind": hold.kind.as_str(),
                "age_secs": age_secs,
            }),
        }
    }
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
        Standing::InFlight { holder } => Some(Tip {
            holder,
            age_secs: match holder {
                Holder::Branch {
                    last_commit_unix, ..
                } => age_secs(now_unix, *last_commit_unix),
                Holder::Claim(hold) => age_secs(now_unix, hold.since),
            },
        }),
        Standing::Frontier | Standing::Blocked(_) | Standing::Closed | Standing::OutOfScope => None,
    }
}

/// One text row of `frontier`: only the name is cut to `width`; the
/// prefix, the due mark and the label are counted and kept whole.
fn frontier_row(prefix: &str, mark: &str, name: &str, label: &str, width: Option<usize>) -> String {
    let used = display_width(prefix) + display_width(mark) + display_width(label);
    let name = fit_name(name, used, width);
    format!("{prefix}{mark}{name}{label}\n")
}

fn ticket_json(
    programme: &Programme,
    ticket: &Ticket,
    tip: Option<&Tip<'_>>,
    today: NaiveDate,
) -> Value {
    json!({
        "id": ticket.id,
        "type": ticket.kind.as_str(),
        "mode": ticket.mode.as_str(),
        "repos": programme.repos_of(ticket),
        "name": ticket.name,
        "branch": tip.and_then(|t| match t.holder {
            Holder::Branch { branch, .. } => Some(branch.as_str()),
            Holder::Claim(_) => None,
        }),
        "branch_age_secs": tip.and_then(|t| match t.holder {
            Holder::Branch { .. } => Some(t.age_secs),
            Holder::Claim(_) => None,
        }),
        "held_by": Tip::json(tip),
        "due": ticket.due.map(|date| date.to_string()),
        "overdue": ticket.due.map(|date| is_overdue(date, today)),
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
    let today = chrono::Local::now().date_naive();
    let mut shown: Vec<(&Ticket, Option<Tip<'_>>)> = programme
        .tickets
        .values()
        .filter_map(|ticket| match (&standings[&ticket.id], listing) {
            (standing, _) if is_ready(standing) => Some((ticket, None)),
            (standing @ Standing::InFlight { .. }, Listing::WithInFlight) => {
                Some((ticket, tip_of(standing, now)))
            }
            (Standing::InFlight { .. }, Listing::ReadyOnly)
            | (
                Standing::Frontier | Standing::Blocked(_) | Standing::Closed | Standing::OutOfScope,
                Listing::ReadyOnly | Listing::WithInFlight,
            ) => None,
        })
        .collect();
    order_frontier(&mut shown, |(ticket, _)| ticket.due);
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
                .map(|(ticket, tip)| ticket_json(&programme, ticket, tip.as_ref(), today))
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
                    (Some(t), _) => format!("\t{}", t.label()),
                    (None, Listing::WithInFlight) => "\t".to_string(),
                    (None, Listing::ReadyOnly) => String::new(),
                };
                let mark = due_mark(ticket.due, today);
                text.push_str(&frontier_row(&prefix, &mark, &ticket.name, &label, width));
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
                    "id": t.id, "name": t.name,
                    "branch": match tip.holder {
                        Holder::Branch { branch, .. } => Some(branch.as_str()),
                        Holder::Claim(_) => None,
                    },
                    "branch_age_secs": match tip.holder {
                        Holder::Branch { .. } => Some(tip.age_secs),
                        Holder::Claim(_) => None,
                    },
                    "held_by": Tip::json(Some(tip)),
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
                        "  {} ({}): {}\n",
                        ticket.name,
                        ticket.id,
                        tip.holder_text()
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
    match fetch {
        Fetch::Origin => fetch_all(std::slice::from_ref(&located.repo)),
        Fetch::LastFetch => {}
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
    match fetch {
        Fetch::Origin => fetch_all(&repos),
        Fetch::LastFetch => {}
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
                // A programme that could not be selected fails the listing's
                // status as the whole repository used to, but not its rows.
                if !listed.unreadable.is_empty() {
                    outcome = CliOutcome::OperationalFailure;
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

/// The session id a claim command writes under: `--session`, else the
/// environment's. The parent-PID fallback names a terminal tab, not a
/// session, and a claim under it would never be released by its owner.
fn writing_session(explicit: Option<&str>) -> Result<String, CliError> {
    use relais::coordinator::{resolve_session, SessionSource};
    if let Some(session) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(session.to_string());
    }
    let resolved = resolve_session();
    match resolved.source {
        SessionSource::ParentPidFallback => Err(CliError::Usage {
            detail: "no session id: pass --session <id>, or set CLAUDE_CODE_SESSION_ID".to_string(),
        }),
        SessionSource::Override | SessionSource::ClaudeCode => Ok(resolved.id),
    }
}

fn claim_store() -> Result<claim::Store, CliError> {
    let state = relais::paths::state_dir().map_err(CliError::Home)?;
    Ok(claim::Store::in_state_dir(&state))
}

fn claim_io(error: claim::ClaimError) -> CliError {
    CliError::Operational {
        operation: "programme claims",
        cause: Box::new(error),
    }
}

/// A claim failure as the command's outcome: a lock that stayed held is
/// exit 1 with the retry command on stderr; anything else is an error.
fn claim_outcome(error: claim::ClaimError, retry: &str) -> Result<CliOutcome, CliError> {
    match error {
        claim::ClaimError::LockTimeout { .. } => {
            eprintln!("relais: {error}");
            eprintln!("next: {retry}");
            Ok(CliOutcome::OperationalFailure)
        }
        claim::ClaimError::Io { .. } => Err(claim_io(error)),
    }
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// A programme read for selection and claiming.
struct Loaded {
    state: claim::ProgrammeState,
}

fn programme_state(found: &Found) -> Result<claim::ProgrammeState, CliError> {
    use relais::programme::model::Status;
    let programme = found.load().map_err(programme_error)?;
    let standings = programme.standings().map_err(programme_error)?;
    let common = load::git_common_dir(&programme.repo_dir).map_err(programme_error)?;
    let mut ready: Vec<&Ticket> =
        relais::programme::frontier::ready_tickets(&programme, &standings).collect();
    order_frontier(&mut ready, |ticket| ticket.due);
    let ids_where = |wanted: fn(Status) -> bool| -> std::collections::BTreeSet<String> {
        programme
            .tickets
            .values()
            .filter(|ticket| wanted(ticket.status))
            .map(|ticket| ticket.id.clone())
            .collect()
    };
    Ok(claim::ProgrammeState {
        repo: dir_name(&programme.repo_dir),
        repo_path: programme.repo_dir.display().to_string(),
        slug: programme.slug.clone(),
        key: claim::ClaimKey {
            repo_key: claim::repo_key(&common),
            slug: programme.slug.clone(),
        },
        ready: ready
            .into_iter()
            .map(|ticket| claim::ReadyTicket {
                id: ticket.id.clone(),
                name: ticket.name.clone(),
                due: ticket.due,
                mode: ticket.mode,
            })
            .collect(),
        open: ids_where(|status| status == Status::Open),
        closed: ids_where(|status| status != Status::Open),
        branch_held: standings
            .iter()
            .filter(|(_, standing)| {
                matches!(
                    standing,
                    Standing::InFlight {
                        holder: Holder::Branch { .. }
                    }
                )
            })
            .map(|(id, _)| id.clone())
            .collect(),
    })
}

/// The programme `slug` of the repository `dir` is inside (default: the
/// current directory), as the claim commands read it.
fn claim_target(slug: &str, dir: Option<&Path>) -> Result<Loaded, CliError> {
    let base = match dir {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir().map_err(CliError::Cwd)?,
    };
    let relative = Path::new("docs").join("programmes").join(slug);
    let main = main_checkout(&base).map_err(programme_error)?;
    let candidates = [base.join(&relative), main.join(&relative)];
    let programme_dir = candidates
        .iter()
        .find(|candidate| candidate.is_dir())
        .unwrap_or(&candidates[1]);
    let found = resolve_dir(programme_dir, Fetch::LastFetch)?;
    Ok(Loaded {
        state: programme_state(&found)?,
    })
}

pub(crate) fn claim_command(
    slug: &str,
    id: &str,
    dir: Option<&Path>,
    session: Option<&str>,
    steal: bool,
) -> Result<CliOutcome, CliError> {
    let session = writing_session(session)?;
    let Loaded { state } = claim_target(slug, dir)?;
    if !state.open.contains(id) {
        return Err(CliError::Usage {
            detail: format!("{}/{slug} has no open ticket {id}", state.repo),
        });
    }
    let store = claim_store()?;
    let now = now_unix();
    store.forget_ended(&session).map_err(claim_io)?;
    let retry = format!(
        "relais programme claim {slug} {id}{}{}",
        dir.map_or_else(String::new, |d| format!(" --dir {}", d.display())),
        if steal { " --steal" } else { "" }
    );
    let taken = store
        .lock(&state.key, claim::LOCK_WAIT)
        .and_then(|lock| {
            lock.sweep(&state.closed, now)?;
            lock.claim_work(id, &session, &state.repo_path, steal, now)
        })
        .and_then(|taken| store.refresh_session(&session, now).map(|_| taken));
    match taken {
        Err(error) => claim_outcome(error, &retry),
        Ok(claim::Taken::Claimed(event)) => {
            eprintln!(
                "relais: {} {} {slug} {id} ({})",
                event.as_str(),
                state.repo,
                state.slug
            );
            Ok(CliOutcome::Accepted)
        }
        Ok(claim::Taken::Refused { holder, age_secs }) => {
            let holder: String = holder.chars().take(8).collect();
            let holder = if holder.is_empty() {
                "an unreadable claim".to_string()
            } else {
                holder
            };
            eprintln!(
                "relais: {}/{slug} {id} is held by {holder} ({} old)",
                state.repo,
                format_age(age_secs)
            );
            eprintln!(
                "next: relais programme next --from {}:{slug} --claim",
                state.repo
            );
            eprintln!(
                "to take it over when the person directs it: relais programme claim {slug} {id}{} --steal",
                dir.map_or_else(String::new, |d| format!(" --dir {}", d.display()))
            );
            Ok(CliOutcome::Blocked)
        }
    }
}

pub(crate) fn release_command(
    slug: &str,
    id: &str,
    dir: Option<&Path>,
    session: Option<&str>,
) -> Result<CliOutcome, CliError> {
    let session = writing_session(session)?;
    let Loaded { state } = claim_target(slug, dir)?;
    let store = claim_store()?;
    let now = now_unix();
    let retry = format!(
        "relais programme release {slug} {id}{}",
        dir.map_or_else(String::new, |d| format!(" --dir {}", d.display()))
    );
    let released = store.lock(&state.key, claim::LOCK_WAIT).and_then(|lock| {
        lock.sweep(&state.closed, now)?;
        lock.release(id, &session, now)
    });
    match released {
        Err(error) => claim_outcome(error, &retry),
        Ok(claim::Released::Released) => {
            eprintln!("relais: released {} {slug} {id}", state.repo);
            Ok(CliOutcome::Accepted)
        }
        Ok(claim::Released::Absent) => Ok(CliOutcome::Accepted),
        Ok(claim::Released::NotHolder { holder }) => {
            let holder: String = holder.chars().take(8).collect();
            eprintln!(
                "relais: {} {slug} {id} is held by {holder}, not this session",
                state.repo
            );
            Ok(CliOutcome::Blocked)
        }
    }
}

pub(crate) fn next_command(
    from: Option<&str>,
    reserve: bool,
    root: Option<&Path>,
    session: Option<&str>,
    format: Format,
) -> Result<CliOutcome, CliError> {
    let from = from
        .map(|spec| {
            spec.split_once(':')
                .filter(|(repo, slug)| !repo.is_empty() && !slug.is_empty())
                .ok_or_else(|| CliError::Usage {
                    detail: format!("--from {spec}: expected <repo>:<slug>"),
                })
        })
        .transpose()?;
    let session = if reserve {
        Some(writing_session(session)?)
    } else {
        None
    };
    let root = match root {
        Some(root) => root.to_path_buf(),
        None => default_root()?,
    };
    let store = claim_store()?;
    let now = now_unix();
    if let Some(session) = &session {
        store.forget_ended(session).map_err(claim_io)?;
    }
    let today = chrono::Local::now().date_naive();
    let all = load::repositories(&root).map_err(programme_error)?;
    let holds = par_map(&all, |repo| load::may_hold_programmes(repo));
    let repos: Vec<PathBuf> = all
        .into_iter()
        .zip(holds)
        .filter_map(|(repo, holds)| holds.then_some(repo))
        .collect();
    let mut founds = Vec::new();
    for (repo, listed) in repos
        .iter()
        .zip(par_map(&repos, |repo| load::programmes_of(repo)))
    {
        match listed {
            Ok(listed) => {
                for note in &listed.notes {
                    eprintln!("relais: {note}");
                }
                founds.extend(listed.found);
            }
            Err(error) => eprintln!("relais: {}: {error}", repo.display()),
        }
    }
    let mut states = Vec::new();
    for (found, state) in founds.iter().zip(par_map(&founds, programme_state)) {
        match state {
            Ok(state) => states.push(state),
            Err(error) => eprintln!("relais: {}: {error}", found.dir().display()),
        }
    }
    let from_state = match from {
        Some((repo, slug)) => {
            let position = states
                .iter()
                .position(|state| state.repo == repo && state.slug == slug)
                .ok_or_else(|| CliError::Usage {
                    detail: format!(
                        "--from {repo}:{slug}: no such programme under {}",
                        root.display()
                    ),
                })?;
            Some(states.remove(position))
        }
        None => None,
    };
    let retry = format!(
        "relais programme next{}{} --root {}",
        from.map_or_else(String::new, |(repo, slug)| format!(" --from {repo}:{slug}")),
        if reserve { " --claim" } else { "" },
        root.display()
    );
    let chosen = claim::next(
        &store,
        from_state.as_ref(),
        &states,
        today,
        now,
        session.as_deref(),
    )
    .and_then(|chosen| match &session {
        Some(session) => store.refresh_session(session, now).map(|_| chosen),
        None => Ok(chosen),
    });
    let chosen = match chosen {
        Ok(chosen) => chosen,
        Err(error) => return claim_outcome(error, &retry),
    };
    match chosen {
        claim::Next::Pick(pick) => {
            let out = match format {
                Format::Json => format!(
                    "{}\n",
                    json!({
                        "repo": pick.repo,
                        "slug": pick.slug,
                        "id": pick.id,
                        "name": pick.name,
                        "claimed": reserve,
                    })
                ),
                Format::Text => format!("{} {} {}\t{}\n", pick.repo, pick.slug, pick.id, pick.name),
            };
            print_stdout(&out)?;
            Ok(CliOutcome::Accepted)
        }
        claim::Next::Parked { next_due } => {
            let line = claim::Next::parked_line(next_due);
            let out = match format {
                Format::Json => format!(
                    "{}\n",
                    json!({
                        "parked": line.trim_start_matches("parked: "),
                        "next_due": next_due.map(|date| date.to_string()),
                    })
                ),
                Format::Text => format!("{line}\n"),
            };
            print_stdout(&out)?;
            Ok(CliOutcome::Parked)
        }
    }
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

    fn day(year: i32, month: u32, date: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, date).expect("a real day")
    }

    #[test]
    fn dated_rows_come_first_soonest_first_then_undated_in_id_order() {
        let mut rows = vec![
            ("A", None),
            ("B", Some(day(2026, 12, 1))),
            ("C", None),
            ("D", Some(day(2026, 11, 1))),
            ("E", Some(day(2026, 12, 1))),
        ];
        order_frontier(&mut rows, |row| row.1);
        let ids: Vec<&str> = rows.iter().map(|row| row.0).collect();
        assert_eq!(ids, ["D", "B", "E", "A", "C"]);
    }

    #[test]
    fn a_narrow_row_cuts_the_due_name_never_its_mark() {
        let prefix = "F\ttask/afk\talpha\t";
        let mark = due_mark(Some(day(2000, 1, 1)), day(2026, 11, 12));
        let long = "A rather long ticket name to cut";
        // 40 columns: the prefix and the mark already fill them, so the name
        // shrinks to the ellipsis and the mark stays whole.
        assert_eq!(
            frontier_row(prefix, &mark, long, "", Some(40)),
            format!("{prefix}[overdue 2000-01-01] …\n")
        );
        // 60 columns: the name is cut to fit, the row is exactly 60 wide.
        let row = frontier_row(prefix, &mark, long, "", Some(60));
        assert!(row.contains("\t[overdue 2000-01-01] A rath"), "{row}");
        assert_eq!(display_width(row.trim_end_matches('\n')), 60, "{row}");
        // No width: nothing is cut.
        assert_eq!(
            frontier_row(prefix, &mark, long, "", None),
            format!("{prefix}[overdue 2000-01-01] {long}\n")
        );
    }

    #[test]
    fn a_due_date_is_overdue_only_before_today() {
        let today = day(2026, 11, 12);
        assert_eq!(due_mark(None, today), "");
        assert_eq!(
            due_mark(Some(day(2026, 11, 11)), today),
            "[overdue 2026-11-11] "
        );
        assert_eq!(due_mark(Some(today), today), "[due 2026-11-12] ");
        assert_eq!(
            due_mark(Some(day(2026, 11, 13)), today),
            "[due 2026-11-13] "
        );
    }

    #[test]
    fn tabs_advance_to_the_next_multiple_of_eight() {
        assert_eq!(display_width("A\tB"), 9);
        assert_eq!(display_width("\t\t"), 16);
    }
}
