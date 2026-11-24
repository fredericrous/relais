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
use relais::programme::frontier::{due_mark, format_age, is_overdue, is_ready, order_frontier};
use relais::programme::load::{self, main_checkout, Found, Programme, ProgrammeError, Source};
use relais::programme::model::Ticket;
use relais::shared_state::node::NodeError;
use serde::Serialize;
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
            Holder::Claim(hold) => format!("claimed {} {age}", hold.holder.short()),
        }
    }

    /// `<branch> <age>` or `claimed <s8> <age>`, as `show` lists it.
    fn holder_text(&self) -> String {
        let age = format_age(self.age_secs);
        match self.holder {
            Holder::Branch { branch, .. } => format!("{branch} {age}"),
            Holder::Claim(hold) => format!("claimed {} {age}", hold.holder.short()),
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
            }) => match &hold.holder {
                claim::Holder::Session { id, kind } => json!({
                    "holder": "claim",
                    "session": id,
                    "kind": kind.as_str(),
                    "age_secs": age_secs,
                }),
                claim::Holder::Unreadable => json!({
                    "holder": "claim",
                    "session": null,
                    "kind": null,
                    "age_secs": age_secs,
                }),
            },
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

/// The claim store the read-only views look in, or `None` when there is no
/// state directory.
fn view_store() -> Option<claim::Store> {
    claim_store().ok() // no home directory: no claim can have been written, so a listing marks nothing held
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
    let now = now_unix();
    let standings = programme
        .standings(view_store().as_ref(), now)
        .map_err(programme_error)?;
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
    let now = now_unix();
    let standings = programme
        .standings(view_store().as_ref(), now)
        .map_err(programme_error)?;
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
    let store = view_store();
    let now = now_unix();
    let loaded_all = par_map(&found_all, |found| {
        found.load().and_then(|programme| {
            programme
                .standings(store.as_ref(), now)
                .map(|s| (programme, s))
        })
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
            detail: "no session id: pass --session <id>, or set RELAIS_SESSION_ID or CLAUDE_CODE_SESSION_ID"
                .to_string(),
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
        claim::ClaimError::Io { .. } | claim::ClaimError::Encode { .. } => Err(claim_io(error)),
    }
}

/// Moves the session's other `work` claims to `now` after a claim command
/// has done its own work. Best effort: the programme locks are waited for
/// up to `ONCE_GRACE` in total, and a failure or a lock still busy is a warning on
/// stderr, never the command's exit status, because the claim the person
/// asked for is already written.
fn refresh_session_best_effort(store: &claim::Store, session: &str, now: i64) {
    match store.refresh_session_once(session, now) {
        Ok(pass) => {
            for line in pass.failure_lines() {
                eprintln!("relais: warning: {line}");
            }
            let busy = pass.skipped.saturating_sub(pass.failures.len());
            if busy > 0 {
                eprintln!(
                    "relais: warning: {busy} programme(s) were busy; your other claims there refresh at your next claim"
                );
            }
        }
        Err(error) => eprintln!("relais: warning: claims: {error}"),
    }
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// `text` as one shell word: unchanged when it holds only characters a
/// shell leaves alone, else single-quoted.
fn shell_quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:=@%+,".contains(c));
    if plain {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// ` --dir <dir>` for a suggested command, or nothing without `--dir`.
fn dir_argument(dir: Option<&Path>) -> String {
    dir.map_or_else(String::new, |dir| {
        format!(" --dir {}", shell_quote(&dir.to_string_lossy()))
    })
}

/// A programme read for selection and claiming, with the directory it was
/// read from.
struct Loaded {
    state: claim::ProgrammeState,
    dir: PathBuf,
}

fn programme_state(
    found: &Found,
    store: &claim::Store,
    now: i64,
) -> Result<claim::ProgrammeState, CliError> {
    use relais::programme::model::Status;
    let programme = found.load().map_err(programme_error)?;
    let standings = programme
        .standings(Some(store), now)
        .map_err(programme_error)?;
    let key = programme.claim_key().map_err(programme_error)?;
    let mut ready: Vec<&Ticket> =
        relais::programme::frontier::ready_tickets(&programme, &standings).collect();
    order_frontier(&mut ready, |ticket| ticket.due);
    Ok(claim::ProgrammeState {
        repo: dir_name(&programme.repo_dir),
        repo_path: programme.repo_dir.display().to_string(),
        slug: programme.slug.clone(),
        key,
        ready: ready
            .into_iter()
            .map(|ticket| claim::ReadyTicket {
                id: ticket.id.clone(),
                name: ticket.name.clone(),
                due: ticket.due,
                mode: ticket.mode,
            })
            .collect(),
        open: programme
            .tickets
            .values()
            .filter(|ticket| ticket.status == Status::Open)
            .map(|ticket| (ticket.id.clone(), ticket.name.clone()))
            .collect(),
        closed: programme
            .tickets
            .values()
            .filter(|ticket| ticket.status != Status::Open)
            .map(|ticket| ticket.id.clone())
            .collect(),
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
fn claim_target(
    slug: &str,
    dir: Option<&Path>,
    store: &claim::Store,
    now: i64,
) -> Result<Loaded, CliError> {
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
        state: programme_state(&found, store, now)?,
        dir: programme_dir.clone(),
    })
}

pub(crate) fn claim_command(
    slug: &str,
    id: &str,
    dir: Option<&Path>,
    session: Option<&str>,
    contest: claim::Contest,
) -> Result<CliOutcome, CliError> {
    let session = writing_session(session)?;
    let store = claim_store()?;
    let now = now_unix();
    let Loaded {
        state,
        dir: programme_dir,
    } = claim_target(slug, dir, &store, now)?;
    let repo = &state.repo;
    if !state.open.contains_key(id) {
        return Err(CliError::Usage {
            detail: format!(
                "{repo} {slug} {id}: no open ticket\nsee: relais programme show {}",
                shell_quote(&programme_dir.to_string_lossy())
            ),
        });
    }
    let steal_flag = match contest {
        claim::Contest::Refuse => "",
        claim::Contest::Steal => " --steal",
    };
    let retry = format!(
        "relais programme claim {slug} {id}{}{steal_flag}",
        dir_argument(dir)
    );
    match take_ticket(&store, &state, id, &session, contest, now)? {
        Err(error) => claim_outcome(error, &retry),
        Ok(claim::Taken::Claimed { how, from }) => {
            eprintln!("relais: {}", claimed_line(&state, slug, id, how, from));
            refresh_session_best_effort(&store, &session, now);
            Ok(CliOutcome::Accepted)
        }
        Ok(claim::Taken::Refused { holder, age_secs }) => {
            eprintln!("relais: {}", held_line(&state, slug, id, &holder, age_secs));
            eprintln!("next: relais programme next --from {repo}:{slug} --claim");
            eprintln!("to take it over (only when the person says so):");
            eprintln!(
                "relais programme claim {slug} {id}{} --steal",
                dir_argument(dir)
            );
            Ok(CliOutcome::Blocked)
        }
    }
}

/// Claims `id` of `state` for `session` as `work`, through the shared state
/// and then the claim file: the core `claim` and `adopt` share. It prints
/// nothing but the one line a shared hold that could not be given back
/// earns. The inner error is a claim-store failure (a lock that stayed
/// held, or I/O), which each caller reports its own way.
fn take_ticket(
    store: &claim::Store,
    state: &claim::ProgrammeState,
    id: &str,
    session: &str,
    contest: claim::Contest,
    now: i64,
) -> Result<Result<claim::Taken, claim::ClaimError>, CliError> {
    store.forget_ended(session).map_err(claim_io)?;
    let holder = relais::shared_state::Principal::operator(session.to_string());
    let taken = relais::shared_state::node::claim_ticket(
        &*super::ctx::shared_state()?,
        store,
        &relais::shared_state::TicketClaim {
            key: &state.key,
            closed: &state.closed,
            id,
            holder: &holder,
            repo_display: &state.repo_path,
            contest,
            now,
        },
    )
    .map(|grant| grant.taken);
    match taken {
        Ok(taken) => Ok(Ok(taken)),
        Err(NodeError::Claim(error)) => Ok(Err(error)),
        // The claim file failed after a shared grant and the grant could
        // not be given back: the claim's own outcome still prints, with its
        // retry command, and the release failure is one more line.
        Err(NodeError::Compensation { local, release }) => match *local {
            NodeError::Claim(error) => {
                eprintln!("relais: the shared hold could not be given back: {release}");
                Ok(Err(error))
            }
            local => {
                let joined = NodeError::Compensation {
                    local: Box::new(local),
                    release,
                };
                Err(super::ctx::node_error("claiming the ticket", joined))
            }
        },
        Err(other) => Err(super::ctx::node_error("claiming the ticket", other)),
    }
}

/// The line a claim that was written prints, after `relais: `.
fn claimed_line(
    state: &claim::ProgrammeState,
    slug: &str,
    id: &str,
    how: claim::Acquired,
    from: Option<claim::Holder>,
) -> String {
    let repo = &state.repo;
    match (how, from) {
        (claim::Acquired::Created, _) => format!("claimed {repo} {slug} {id}"),
        (claim::Acquired::TookOver, Some(from)) => {
            format!("took over {repo} {slug} {id} from {}", from.short())
        }
        (claim::Acquired::TookOver, None) => format!("took over {repo} {slug} {id}"),
        (claim::Acquired::Refreshed, _) => format!("refreshed your claim on {repo} {slug} {id}"),
        (claim::Acquired::Stole, Some(from)) => {
            format!("took {repo} {slug} {id} from {} (--steal)", from.short())
        }
        (claim::Acquired::Stole, None) => format!("took {repo} {slug} {id} (--steal)"),
    }
}

/// The line a refused claim prints first, after `relais: `.
fn held_line(
    state: &claim::ProgrammeState,
    slug: &str,
    id: &str,
    holder: &claim::Holder,
    age_secs: u64,
) -> String {
    format!(
        "{} {slug} {id} is held by {} ({} old)",
        state.repo,
        holder.short(),
        format_age(age_secs)
    )
}

/// `relais programme adopt`: every live `handoff` claim of `previous` (the
/// session a `/clear` ended) becomes this session's `work`, through the same
/// path as `relais programme claim`, so the relayed session holds its ticket
/// whether or not its agent claims it (follow-ups f12). Each ticket is tried
/// on its own; one line per ticket on stderr, nothing on stdout. A ticket
/// that can never be adopted (closed, gone, its repository gone or another
/// repository's) is a line
/// and exit 0, since a retry cannot change it; exit 1 is kept for what a
/// retry may fix (a lock that stayed held, a programme or store that could
/// not be read).
pub(crate) fn adopt_command(previous: &str, session: Option<&str>) -> Result<CliOutcome, CliError> {
    let session = writing_session(session)?;
    let previous = previous.trim();
    if previous.is_empty() {
        return Err(CliError::Usage {
            detail: "--previous is empty; pass the session the /clear ended".to_string(),
        });
    }
    if previous == session {
        return Err(CliError::Usage {
            detail: "--previous is this session; pass the session the /clear ended".to_string(),
        });
    }
    let store = claim_store()?;
    let now = now_unix();
    let handoffs: Vec<claim::SessionClaim> = store
        .claims_of(previous, now)
        .map_err(claim_io)?
        .live
        .into_iter()
        .filter(|found| found.claim.kind == claim::Kind::Handoff)
        .collect();
    let mut failed = false;
    for found in &handoffs {
        let (slug, id) = (found.key.slug.as_str(), found.id.as_str());
        let recorded = Path::new(&found.claim.repo);
        let named = format!("{} {slug} {id}", dir_name(recorded));
        let Some(dir) = relais::learn::salvage::reachable_repo(recorded) else {
            eprintln!(
                "relais: {named} not adopted: its repository is not found ({})",
                found.claim.repo
            );
            continue;
        };
        let state = match claim_target(slug, Some(&dir), &store, now) {
            Ok(loaded) if loaded.state.key == found.key => loaded.state,
            Ok(_) => {
                eprintln!(
                    "relais: {named} not adopted: {} is not the repository that holds the claim",
                    dir.display()
                );
                continue;
            }
            Err(error) => {
                eprintln!("relais: {named} not adopted: its programme cannot be read: {error}");
                failed = true;
                continue;
            }
        };
        if !state.open.contains_key(id) {
            let why = if state.closed.contains(id) {
                "skipped: the ticket is closed"
            } else {
                "not adopted: no such ticket"
            };
            eprintln!("relais: {} {slug} {id} {why}", state.repo);
            continue;
        }
        match take_ticket(&store, &state, id, &session, claim::Contest::Refuse, now) {
            Ok(Ok(claim::Taken::Claimed { how, from })) => {
                eprintln!("relais: {}", claimed_line(&state, slug, id, how, from));
            }
            Ok(Ok(claim::Taken::Refused { holder, age_secs })) => {
                eprintln!("relais: {}", held_line(&state, slug, id, &holder, age_secs));
            }
            // A store failure and an operational one read alike to a person:
            // the ticket was not adopted, and a retry may take it.
            Ok(Err(error)) => {
                eprintln!("relais: {} {slug} {id} not adopted: {error}", state.repo);
                failed = true;
            }
            Err(error) => {
                eprintln!("relais: {} {slug} {id} not adopted: {error}", state.repo);
                failed = true;
            }
        }
    }
    Ok(if failed {
        CliOutcome::OperationalFailure
    } else {
        CliOutcome::Accepted
    })
}

/// Why the null answer of `programme session` has no programme.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Absence {
    None,
    Expired,
}

/// The answer of `programme session` when the session holds no claim.
#[derive(Serialize)]
struct NoProgrammeView {
    programme: Option<()>,
    reason: Absence,
}

#[derive(Serialize)]
struct SessionAnswer<T: Serialize> {
    programme: T,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum StatusView {
    Closed,
    Frontier,
    Blocked,
    InFlight,
    OutOfScope,
}

impl From<&Standing> for StatusView {
    fn from(standing: &Standing) -> Self {
        match standing {
            Standing::Frontier => Self::Frontier,
            Standing::Blocked(_) => Self::Blocked,
            Standing::InFlight { .. } => Self::InFlight,
            Standing::Closed => Self::Closed,
            Standing::OutOfScope => Self::OutOfScope,
        }
    }
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum KindView {
    Work,
    Handoff,
}

impl From<claim::Kind> for KindView {
    fn from(kind: claim::Kind) -> Self {
        match kind {
            claim::Kind::Work => Self::Work,
            claim::Kind::Handoff => Self::Handoff,
        }
    }
}

impl KindView {
    fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Handoff => "handoff",
        }
    }
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum HolderView {
    #[serde(rename = "self")]
    Own,
    Previous,
}

impl From<claim::Claimant> for HolderView {
    fn from(claimant: claim::Claimant) -> Self {
        match claimant {
            claim::Claimant::Own => Self::Own,
            claim::Claimant::Previous => Self::Previous,
        }
    }
}

#[derive(Serialize)]
struct CountsView {
    open: usize,
    frontier: usize,
    blocked: usize,
    in_flight: usize,
    closed: usize,
    out_of_scope: usize,
}

impl From<Counts> for CountsView {
    fn from(counts: Counts) -> Self {
        Self {
            open: counts.open,
            frontier: counts.frontier,
            blocked: counts.blocked,
            in_flight: counts.in_flight,
            closed: counts.closed,
            out_of_scope: counts.out_of_scope,
        }
    }
}

#[derive(Serialize)]
struct TicketView {
    id: String,
    name: String,
    status: StatusView,
    blocked_by: Vec<String>,
    open_blockers: Vec<String>,
    dependents: Vec<String>,
    due: Option<String>,
    held: bool,
}

/// A held claim whose map was read.
#[derive(Serialize)]
struct MapView {
    repo: String,
    slug: String,
    ticket: String,
    kind: KindView,
    holder: HolderView,
    counts: CountsView,
    tickets: Vec<TicketView>,
    ready: Vec<String>,
}

/// A held claim whose map could not be read.
#[derive(Serialize)]
struct FailedView {
    repo: String,
    slug: String,
    ticket: String,
    kind: KindView,
    error: String,
}

/// Why the map of a claimed programme could not be read.
enum MapFailure {
    RepoGone,
    NotInMap { id: String },
    Programme(CliError),
}

impl std::fmt::Display for MapFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RepoGone => f.write_str("the repository is gone"),
            Self::NotInMap { id } => write!(f, "ticket {id} is not in the map"),
            Self::Programme(error) => write!(f, "{error}"),
        }
    }
}

impl From<CliError> for MapFailure {
    fn from(error: CliError) -> Self {
        Self::Programme(error)
    }
}

impl From<ProgrammeError> for MapFailure {
    fn from(error: ProgrammeError) -> Self {
        Self::Programme(programme_error(error))
    }
}

/// Where the map of a claimed programme is, in order: the programme under
/// `dir` (or its main checkout) when `dir` is in the claim's repository (by
/// repo key); else under the repository path the claim recorded, when it
/// holds the programme; else under that path's main checkout.
fn session_map_dir(held: &claim::SessionClaim, dir: Option<&Path>) -> Result<PathBuf, MapFailure> {
    let relative = Path::new("docs").join("programmes").join(&held.key.slug);
    if let Some(dir) = dir {
        let same_repo = load::git_common_dir(dir)
            .is_ok_and(|common| claim::repo_key(&common) == held.key.repo_key);
        if same_repo {
            let here = dir.join(&relative);
            if here.is_dir() {
                return Ok(here);
            }
            if let Ok(main) = main_checkout(dir) {
                let in_main = main.join(&relative);
                if in_main.is_dir() {
                    return Ok(in_main);
                }
            }
        }
    }
    let recorded = Path::new(&held.claim.repo);
    if held.claim.repo.is_empty() || !recorded.exists() {
        return Err(MapFailure::RepoGone);
    }
    let here = recorded.join(&relative);
    if here.is_dir() {
        return Ok(here);
    }
    Ok(main_checkout(recorded)?.join(relative))
}

fn session_map_view(
    held: &claim::SessionClaim,
    claimant: claim::Claimant,
    programme_dir: &Path,
    store: &claim::Store,
    fetch: Fetch,
    now: i64,
) -> Result<MapView, MapFailure> {
    let found = resolve_dir(programme_dir, fetch)?;
    let programme = found.load()?;
    let standings = programme.standings(Some(store), now)?;
    if !programme.tickets.contains_key(&held.id) {
        return Err(MapFailure::NotInMap {
            id: held.id.clone(),
        });
    }
    let mut ready: Vec<&Ticket> =
        relais::programme::frontier::ready_tickets(&programme, &standings).collect();
    order_frontier(&mut ready, |ticket| ticket.due);
    let relations = analysis::relations(&programme.tickets, &standings);
    let tickets = programme
        .tickets
        .values()
        .map(|ticket| {
            let standing = &standings[&ticket.id];
            let related = relations.get(&ticket.id).cloned().unwrap_or_default();
            TicketView {
                id: ticket.id.clone(),
                name: ticket.name.clone(),
                status: StatusView::from(standing),
                blocked_by: ticket.blocked_by.clone(),
                open_blockers: related.open_blockers,
                dependents: related.dependents,
                due: ticket.due.map(|date| date.to_string()),
                held: matches!(standing, Standing::InFlight { .. }),
            }
        })
        .collect();
    Ok(MapView {
        repo: dir_name(&programme.repo_dir),
        slug: held.key.slug.clone(),
        ticket: held.id.clone(),
        kind: held.claim.kind.into(),
        holder: claimant.into(),
        counts: analysis::counts(&standings).into(),
        tickets,
        ready: ready.iter().map(|ticket| ticket.id.clone()).collect(),
    })
}

fn json_line<T: Serialize>(answer: &T) -> Result<String, CliError> {
    serde_json::to_string(answer)
        .map(|text| format!("{text}\n"))
        .map_err(|cause| CliError::Operational {
            operation: "programme session: encoding the answer",
            cause: Box::new(cause),
        })
}

/// `relais programme session`: which programme ticket `session` holds by
/// claim, with the programme's tickets, for the plugin's pane. Read-only:
/// no claim is swept, refreshed or written.
pub(crate) fn session_command(
    session: Option<&str>,
    previous: Option<&str>,
    dir: Option<&Path>,
    format: Format,
    fetch: Fetch,
) -> Result<CliOutcome, CliError> {
    let session = match writing_session(session) {
        Ok(session) => session,
        Err(CliError::Usage { .. }) => {
            eprintln!("relais: no session id");
            eprintln!("next: pass --session <id>");
            return Ok(CliOutcome::InvalidInput);
        }
        Err(other) => return Err(other),
    };
    let store = claim_store()?;
    let now = now_unix();
    let own = store.claims_of(&session, now).map_err(claim_io)?;
    let before = match previous.map(str::trim).filter(|p| !p.is_empty()) {
        Some(previous) => Some(store.claims_of(previous, now).map_err(claim_io)?),
        None => None,
    };
    let (held, claimant) = match claim::resolve_session(&own, before.as_ref()) {
        claim::Resolution::Claimed { claim, claimant } => (claim, claimant),
        claim::Resolution::Nothing { expired } => {
            let reason = if expired {
                Absence::Expired
            } else {
                Absence::None
            };
            let out = match (format, reason) {
                (Format::Json, reason) => json_line(&NoProgrammeView {
                    programme: None,
                    reason,
                })?,
                (Format::Text, Absence::Expired) => "programme claim expired\n".to_string(),
                (Format::Text, Absence::None) => "no programme claim\n".to_string(),
            };
            print_stdout(&out)?;
            return Ok(CliOutcome::Accepted);
        }
    };
    let cwd = std::env::current_dir().ok(); // only locates the map: without it the claim's own path is tried
    let dir = dir.map(Path::to_path_buf).or(cwd);
    let map = session_map_dir(&held, dir.as_deref()).and_then(|programme_dir| {
        session_map_view(&held, claimant, &programme_dir, &store, fetch, now)
    });
    match map {
        Ok(view) => {
            let out = match format {
                Format::Json => json_line(&SessionAnswer { programme: &view })?,
                Format::Text => format!("{} {} ({})\n", view.slug, view.ticket, view.kind.as_str()),
            };
            print_stdout(&out)?;
            Ok(CliOutcome::Accepted)
        }
        Err(failure) => {
            let repo = dir_name(Path::new(&held.claim.repo));
            let (slug, ticket) = (&held.key.slug, &held.id);
            let kind = KindView::from(held.claim.kind);
            match format {
                Format::Json => {
                    let out = json_line(&SessionAnswer {
                        programme: FailedView {
                            repo,
                            slug: slug.clone(),
                            ticket: ticket.clone(),
                            kind,
                            error: failure.to_string(),
                        },
                    })?;
                    print_stdout(&out)?;
                    Ok(CliOutcome::Accepted)
                }
                Format::Text => {
                    print_stdout(&format!("{slug} {ticket} ({})\n", kind.as_str()))?;
                    eprintln!("relais: {repo} {slug}: {failure}");
                    // The session is named, so the hint releases this
                    // session's claim whatever the shell's environment holds.
                    let release = format!(
                        "relais programme release {} {} --session {}",
                        shell_quote(slug),
                        shell_quote(ticket),
                        shell_quote(&session)
                    );
                    let map_dir =
                        shell_quote(&format!("{}/docs/programmes/{slug}", held.claim.repo));
                    match &failure {
                        // `release` finds the map through `--dir` too, so both
                        // choices need a checkout of the repository.
                        MapFailure::RepoGone => eprintln!(
                            "next: rerun with --dir <a checkout of {repo}>, or {release} --dir <a checkout of {repo}>"
                        ),
                        MapFailure::NotInMap { .. } => {
                            eprintln!("next: fix the map at {map_dir}, or {release}")
                        }
                        MapFailure::Programme(_) => {
                            eprintln!("next: relais programme show {map_dir}")
                        }
                    }
                    Ok(CliOutcome::OperationalFailure)
                }
            }
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
    let store = claim_store()?;
    let now = now_unix();
    let Loaded { state, .. } = claim_target(slug, dir, &store, now)?;
    let repo = &state.repo;
    let retry = format!("relais programme release {slug} {id}{}", dir_argument(dir));
    let released = store
        .lock(&state.key, claim::LOCK_WAIT, now)
        .and_then(|lock| {
            lock.sweep(&state.closed, now)?;
            lock.release(id, &session, now)
        });
    match released {
        Err(error) => claim_outcome(error, &retry),
        Ok(claim::Released::Released) => {
            eprintln!("relais: released {repo} {slug} {id}");
            Ok(CliOutcome::Accepted)
        }
        Ok(claim::Released::Absent) => Ok(CliOutcome::Accepted),
        Ok(claim::Released::NotHolder { holder }) => {
            eprintln!(
                "relais: {repo} {slug} {id} is held by {}, not this session",
                holder.short()
            );
            let ends = match &holder {
                claim::Holder::Session {
                    kind: claim::Kind::Handoff,
                    ..
                } => format!(
                    "when it is taken over or after {} h",
                    claim::HANDOFF_HOLD_SECS / 3600
                ),
                claim::Holder::Session {
                    kind: claim::Kind::Work,
                    ..
                } => format!(
                    "at its holder's SessionEnd or after {} h",
                    claim::WORK_HOLD_SECS / 3600
                ),
                claim::Holder::Unreadable => format!(
                    "once its file is {} s old, when it stops counting",
                    claim::UNPARSEABLE_HOLD_SECS
                ),
            };
            eprintln!("next: nothing to release; it ends {ends}");
            Ok(CliOutcome::Blocked)
        }
    }
}

/// Whether `next` only names a ticket or also claims it for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reservation {
    Look,
    Claim,
}

impl Reservation {
    pub(crate) fn from_claim_flag(claim: bool) -> Self {
        if claim {
            Self::Claim
        } else {
            Self::Look
        }
    }
}

pub(crate) fn next_command(
    from: Option<&str>,
    reservation: Reservation,
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
    let reserving = match reservation {
        Reservation::Claim => Some(writing_session(session)?),
        Reservation::Look => None,
    };
    let intent =
        reserving
            .as_deref()
            .map_or(claim::Intent::Look, |session| claim::Intent::Reserve {
                session,
            });
    let root = match root {
        Some(root) => root.to_path_buf(),
        None => default_root()?,
    };
    let store = claim_store()?;
    let now = now_unix();
    if let Some(session) = &reserving {
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
    let mut load_failed = false;
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
                load_failed |= !listed.unreadable.is_empty();
                founds.extend(listed.found);
            }
            Err(error) => {
                eprintln!("relais: {}: {error}", repo.display());
                load_failed = true;
            }
        }
    }
    let mut states = Vec::new();
    for (found, state) in founds.iter().zip(par_map(&founds, |found| {
        programme_state(found, &store, now)
    })) {
        match state {
            Ok(state) => states.push(state),
            Err(error) => {
                eprintln!("relais: {}: {error}", found.dir().display());
                load_failed = true;
            }
        }
    }
    let from_state = match from {
        Some((repo, slug)) => {
            let Some(position) = states
                .iter()
                .position(|state| state.repo == repo && state.slug == slug)
            else {
                if load_failed {
                    eprintln!(
                        "relais: --from {repo}:{slug}: its programme failed to load (see above)"
                    );
                    return Ok(CliOutcome::OperationalFailure);
                }
                return Err(CliError::Usage {
                    detail: format!(
                        "--from {repo}:{slug}: no such programme under {}",
                        root.display()
                    ),
                });
            };
            Some(states.remove(position))
        }
        None => None,
    };
    let retry = format!(
        "relais programme next{}{} --root {}",
        from.map_or_else(String::new, |(repo, slug)| format!(" --from {repo}:{slug}")),
        match reservation {
            Reservation::Claim => " --claim",
            Reservation::Look => "",
        },
        shell_quote(&root.to_string_lossy())
    );
    let chosen = claim::next(&store, from_state.as_ref(), &states, today, now, intent);
    let chosen = match chosen {
        Ok(chosen) => chosen,
        Err(error) => return claim_outcome(error, &retry),
    };
    if let Some(session) = &reserving {
        refresh_session_best_effort(&store, session, now);
    }
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
                        "claimed": reservation == Reservation::Claim,
                    })
                ),
                Format::Text => format!("{} {} {}\t{}\n", pick.repo, pick.slug, pick.id, pick.name),
            };
            print_stdout(&out)?;
            Ok(CliOutcome::Accepted)
        }
        claim::Next::Parked { .. } if load_failed => {
            eprintln!("relais: not parking: a programme failed to load (see above)");
            Ok(CliOutcome::OperationalFailure)
        }
        claim::Next::Parked { next_due } => {
            let out = match format {
                Format::Json => format!(
                    "{}\n",
                    json!({
                        "parked": true,
                        "next_due": next_due.map(|date| date.to_string()),
                    })
                ),
                Format::Text => format!("{}\n", claim::Next::parked_line(next_due)),
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
