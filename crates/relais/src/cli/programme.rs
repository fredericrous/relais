//! `relais programme list|frontier|show|check` (SPEC §31): read-only
//! queries over programme maps. They open no ledger, no coordinator and no
//! machine config; the only outside reads are the programme's files and
//! the branch names git lists in the map's repository.

use std::io::Write;
use std::path::{Path, PathBuf};

use relais::programme::analysis::{self, age_secs, Counts, Standing};
use relais::programme::load::{find_programmes, main_checkout, Programme, ProgrammeError};
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
        .unwrap_or(0)
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
) -> Result<CliOutcome, CliError> {
    let programme = Programme::load(dir).map_err(programme_error)?;
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
                let label = tip
                    .as_ref()
                    .map(|t| format!("\tin-flight {} {}", t.branch, format_age(t.age_secs)))
                    .unwrap_or_default();
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

pub(crate) fn show_command(dir: &Path, format: Format) -> Result<CliOutcome, CliError> {
    let programme = Programme::load(dir).map_err(programme_error)?;
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
            let mut text = format!("Destination\n{}\n\n", programme.map.destination);
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

pub(crate) fn check_command(dir: &Path, format: Format) -> Result<CliOutcome, CliError> {
    let programme = Programme::load(dir).map_err(programme_error)?;
    let problems = programme.problems();
    for problem in &problems {
        eprintln!(
            "{}:{}: {}",
            dir.join(&problem.file).display(),
            problem.line,
            problem.message
        );
    }
    if !problems.is_empty() {
        eprintln!("{} problems", problems.len());
    }
    if format == Format::Json {
        let rows: Vec<Value> = problems
            .iter()
            .map(|p| {
                json!({
                    "path": dir.join(&p.file).display().to_string(),
                    "line": p.line,
                    "message": p.message,
                })
            })
            .collect();
        print_stdout(&format!("{}\n", json!({ "problems": rows })))?;
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

pub(crate) fn list_command(root: Option<&Path>, format: Format) -> Result<CliOutcome, CliError> {
    let root = match root {
        Some(root) => root.to_path_buf(),
        None => default_root()?,
    };
    let mut rows: Vec<(PathBuf, String, Counts)> = Vec::new();
    let mut outcome = CliOutcome::Accepted;
    for dir in find_programmes(&root).map_err(programme_error)? {
        let loaded = Programme::load(&dir)
            .and_then(|programme| programme.standings().map(|s| (programme, s)));
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
                .map(|(path, destination, counts)| {
                    json!({
                        "path": path.display().to_string(),
                        "destination": destination,
                        "open": counts.open,
                        "frontier": counts.frontier,
                        "closed": counts.closed,
                    })
                })
                .collect();
            format!("{}\n", Value::Array(items))
        }
        Format::Text => rows
            .iter()
            .map(|(path, destination, counts)| {
                format!(
                    "{}\t{destination}\topen {} frontier {} closed {}\n",
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
