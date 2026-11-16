//! `relais report runs --json`: when each run started and ended, and for
//! how long, as a document another tool can join on.
//!
//! greenbelay's time-to-done measure reads this and nothing of relais's
//! ledger file (`harness.cross-tool-reads`), so the shape carries a
//! `schema` token: a change that breaks a reader bumps it.

use serde::Serialize;

use crate::ledger::{Ledger, LedgerError, RunWindow};

/// The version of [`RunsDocument`]. A reader that sees another number
/// treats the document as one from a tool it does not know.
pub const RUNS_SCHEMA: u32 = 1;

#[derive(Debug, Serialize, PartialEq)]
pub struct RunsDocument {
    pub schema: u32,
    pub since: String,
    pub runs: Vec<RunSpan>,
}

/// One root run. `ended_at` is `null` while the run goes on, and `hours`
/// then runs to the `now` the document was built at.
#[derive(Debug, Serialize, PartialEq)]
pub struct RunSpan {
    pub id: String,
    pub repo: String,
    pub root_session: Option<String>,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub hours: f64,
}

/// The runs created at or after `since`, with `now` the end of any run
/// still going. `now` is a parameter so a test does not depend on the
/// clock.
pub fn runs_document(
    ledger: &Ledger,
    since: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<RunsDocument, LedgerError> {
    let runs = ledger
        .runs_window(since)?
        .into_iter()
        .map(|window| span(window, now))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RunsDocument {
        schema: RUNS_SCHEMA,
        since: since.to_string(),
        runs,
    })
}

fn span(window: RunWindow, now: chrono::DateTime<chrono::Utc>) -> Result<RunSpan, LedgerError> {
    let parse = |what: &str, text: &str| {
        chrono::DateTime::parse_from_rfc3339(text)
            .map(|at| at.with_timezone(&chrono::Utc))
            .map_err(|error| LedgerError::Corrupt {
                what: format!("{what} of run {}", window.id.as_str()),
                detail: format!("{text:?}: {error}"),
            })
    };
    let started = parse("the start", &window.started_at)?;
    let ended = match &window.ended_at {
        Some(text) => parse("the end", text)?,
        None => now,
    };
    // A clock that stepped back, or a run stamped after `now`, is zero
    // hours, never a negative one.
    let seconds = (ended - started).num_milliseconds().max(0) as f64 / 1000.0;
    Ok(RunSpan {
        id: window.id.as_str().to_string(),
        repo: window.repo,
        root_session: window.root_session,
        status: window.status.as_str().to_string(),
        started_at: window.started_at,
        ended_at: window.ended_at,
        hours: (seconds / 3600.0 * 1000.0).round() / 1000.0,
    })
}
