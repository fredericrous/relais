//! Writing a described set of runs into a fresh ledger (`relais dataset
//! seed`, library side).
//!
//! The runs go through the ledger's ordinary write functions into an
//! exclusively created, invocation-specific staging database beside the
//! destination. The staging file is closed (WAL checkpointed away) and then
//! published with `hard_link` onto the absent `ledger.sqlite`: the link is
//! the commit point, and it fails rather than replaces when the
//! destination exists. Nothing here opens, renames over or removes a
//! destination, and nothing removes another invocation's staging files.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ids::{RunId, TaskId};
use crate::ledger::{FixedClock, HoldoutOutcomeRow, Ledger, LedgerError, Transition, UsageEvent};
use crate::lifecycle::{Reason, State, UsagePhase};
use crate::money::{CostCompleteness, CostKind, MicroUsd};

use crate::paths::{LEDGER_FILE, SHM_SUFFIX, WAL_SUFFIX};

/// The repository path and key every seeded run carries.
const SEED_REPO_PATH: &str = "/seed";
const SEED_REPO_KEY: &str = "seed";

/// Who accepted an `accepted` run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptedBy {
    Relais,
    Person,
}

/// The state a seeded run ends in; only `Accepted` carries data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedStatus {
    Accepted(AcceptedBy),
    Failed,
    Blocked,
    NeedsDecision,
    NeedsReview,
    Interrupted,
    BudgetExhausted,
    Cancelled,
}

impl SeedStatus {
    fn final_state(self) -> State {
        match self {
            Self::Accepted(_) => State::Accepted,
            Self::Failed => State::Failed,
            Self::Blocked => State::Blocked,
            Self::NeedsDecision => State::NeedsDecision,
            Self::NeedsReview => State::NeedsReview,
            Self::Interrupted => State::Interrupted,
            Self::BudgetExhausted => State::BudgetExhausted,
            Self::Cancelled => State::Cancelled,
        }
    }

    fn final_reason(self) -> Reason {
        match self {
            Self::Accepted(AcceptedBy::Relais) => Reason::ChecksAndReviewPassed,
            Self::Accepted(AcceptedBy::Person) => Reason::DecisionApproved,
            Self::Failed => Reason::BehavioralFailure,
            Self::Blocked => Reason::BlockedPreflight,
            Self::NeedsDecision => Reason::HoldoutFailed,
            Self::NeedsReview => Reason::ReviewFindings,
            Self::Interrupted => Reason::ReconciledInterrupted,
            Self::BudgetExhausted => Reason::LimitReached,
            Self::Cancelled => Reason::CancelledByUser,
        }
    }
}

/// What a holdout verification concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldoutVerdict {
    Passed,
    Failed,
    DidNotBuild,
    TimedOut,
    Skipped,
}

impl HoldoutVerdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::DidNotBuild => "did_not_build",
            Self::TimedOut => "timed_out",
            Self::Skipped => "skipped",
        }
    }
}

/// Where a holdout's definition came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldoutSource {
    Named,
    Attached,
}

impl HoldoutSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Named => "named",
            Self::Attached => "attached",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedHoldout {
    pub name: String,
    pub outcome: HoldoutVerdict,
    pub profile_green: bool,
    pub source: HoldoutSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedExposure {
    pub marker: String,
}

/// One validated run to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedRun {
    pub run: String,
    pub task: String,
    /// RFC 3339; the ledger clock for every row of this run.
    pub created_at: String,
    pub status: SeedStatus,
    pub cost_micros: Option<i64>,
    pub holdouts: Vec<SeedHoldout>,
    pub exposures: Vec<SeedExposure>,
}

/// How a seed write ended when it did not fail.
#[derive(Debug, PartialEq, Eq)]
pub enum SeedOutcome {
    /// The ledger is in place at `path`, the name the writer linked.
    /// `cleanup_warning` is set when removing the
    /// staging name failed after the link; the destination is complete
    /// regardless.
    Published {
        path: PathBuf,
        cleanup_warning: Option<String>,
    },
    /// `ledger.sqlite` already exists; nothing was written to it.
    DestinationExists { path: PathBuf },
}

#[derive(Debug)]
pub enum SeedError {
    Ledger(LedgerError),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl std::fmt::Display for SeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ledger(error) => write!(f, "ledger: {error}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "{operation} {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for SeedError {}

impl From<LedgerError> for SeedError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error)
    }
}

/// The staging database of one invocation, and the sidecars SQLite may
/// put beside it. Until published, dropping removes exactly these files.
struct Staging {
    database: PathBuf,
    wal: PathBuf,
    shm: PathBuf,
    published: bool,
}

impl Staging {
    /// Create this invocation's staging file, exclusively, in `state_dir`.
    fn create(state_dir: &Path) -> Result<Self, SeedError> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(state_dir).map_err(|source| SeedError::Io {
            operation: "create state directory",
            path: state_dir.to_path_buf(),
            source,
        })?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let name = format!(
            ".{LEDGER_FILE}.seed-{}-{nanos}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let database = state_dir.join(&name);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&database)
            .map_err(|source| SeedError::Io {
                operation: "create staging database",
                path: database.clone(),
                source,
            })?;
        Ok(Self {
            wal: state_dir.join(format!("{name}{WAL_SUFFIX}")),
            shm: state_dir.join(format!("{name}{SHM_SUFFIX}")),
            database,
            published: false,
        })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        for path in [&self.database, &self.wal, &self.shm] {
            // Drop must neither panic nor replace the primary error; a
            // file that is already gone is the goal, anything else is
            // reported to stderr and left for the person.
            if let Err(error) = std::fs::remove_file(path) {
                if error.kind() != io::ErrorKind::NotFound {
                    eprintln!(
                        "relais dataset seed: could not remove {}: {error}",
                        path.display()
                    );
                }
            }
        }
    }
}

/// Write `runs` into a new ledger in `state_dir`. Either the complete
/// ledger appears as `ledger.sqlite`, or nothing of this invocation
/// remains (bar a cleanup warning after publication).
pub fn write_seed(state_dir: &Path, runs: &[SeedRun]) -> Result<SeedOutcome, SeedError> {
    let mut staging = Staging::create(state_dir)?;

    // An empty file is not a ledger: migrate it once, so a seed of no
    // runs still publishes a valid, empty one.
    Ledger::open(&staging.database)?.close()?;
    for (index, run) in runs.iter().enumerate() {
        write_run(&staging.database, run)?;
        #[cfg(test)]
        if index == 0 {
            tests::hooks::after_first_run();
        }
        #[cfg(not(test))]
        let _ = index;
    }

    if staging.wal.exists() {
        return Err(SeedError::Io {
            operation: "close staging database: write-ahead log remains",
            path: staging.wal.clone(),
            source: io::Error::other("the checkpoint left a -wal file"),
        });
    }

    let destination = state_dir.join(LEDGER_FILE);
    match std::fs::hard_link(&staging.database, &destination) {
        Ok(()) => staging.published = true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Ok(SeedOutcome::DestinationExists { path: destination });
        }
        Err(source) => {
            return Err(SeedError::Io {
                operation: "publish ledger",
                path: destination,
                source,
            });
        }
    }

    let cleanup_warning = remove_staging_name(&staging.database).err().map(|error| {
        format!(
            "could not remove the staging file {}: {error}",
            staging.database.display()
        )
    });
    Ok(SeedOutcome::Published {
        path: destination,
        cleanup_warning,
    })
}

fn remove_staging_name(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    if tests::hooks::take_cleanup_failure() {
        return Err(io::Error::other("injected cleanup failure"));
    }
    std::fs::remove_file(path)
}

/// One run through a ledger whose clock stands at its `created_at`.
fn write_run(database: &Path, run: &SeedRun) -> Result<(), SeedError> {
    let ledger = Ledger::open_with_clock(
        database,
        Box::new(FixedClock::new([run.created_at.clone()])),
    )?;
    let run_id = RunId::from_stored(run.run.clone());
    ledger.insert_run(
        &run_id,
        SEED_REPO_PATH,
        None,
        &TaskId::from_stored(run.task.clone()),
        SEED_REPO_KEY,
    )?;
    let step = |from: State, to: State, reason: Reason| Transition {
        run_id: run_id.clone(),
        attempt_id: None,
        from_state: Some(from),
        to_state: to,
        reason: reason.as_str().to_string(),
        detail: None,
        at: ledger.now(),
    };
    ledger.record_transition(&step(
        State::Prepared,
        State::Verifying,
        Reason::VerificationStarted,
    ))?;
    ledger.record_transition(&step(
        State::Verifying,
        run.status.final_state(),
        run.status.final_reason(),
    ))?;
    if let Some(micros) = run.cost_micros {
        ledger.record_usage(&UsageEvent {
            event_id: format!("seed-{}", run.run),
            run_id: run_id.clone(),
            attempt_id: None,
            parent_event_id: None,
            model: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost: Some(MicroUsd::from_micros(micros)),
            cost_kind: CostKind::ApiSpend,
            completeness: CostCompleteness::Actual,
            inclusive: false,
            at: run.created_at.clone(),
            phase: Some(UsagePhase::Initial),
            duration_ms: None,
            requested_model: None,
            requested_effort: None,
            harness: None,
        })?;
    }
    for holdout in &run.holdouts {
        ledger.record_holdout_outcome(
            &run_id,
            &HoldoutOutcomeRow {
                label_kind: "attempt",
                attempt_index: 1,
                name: &holdout.name,
                outcome: holdout.outcome.as_str(),
                profile_green: holdout.profile_green,
                source: holdout.source.as_str(),
            },
        )?;
    }
    for exposure in &run.exposures {
        ledger.record_holdout_exposure(&run_id, "seed", "seed", &exposure.marker)?;
    }
    ledger.close()?;
    Ok(())
}

/// The seed file's schema version, the only one this build reads.
const SEED_SCHEMA: i64 = 1;

/// One refused line of a seed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedProblem {
    pub line: usize,
    pub reason: String,
}

fn parse_status(status: &str, accepted_by: Option<&str>) -> Result<SeedStatus, String> {
    if accepted_by.is_some() && status != "accepted" {
        return Err("accepted_by applies to status accepted only".into());
    }
    Ok(match status {
        "accepted" => SeedStatus::Accepted(match accepted_by {
            None | Some("relais") => AcceptedBy::Relais,
            Some("person") => AcceptedBy::Person,
            Some(other) => {
                return Err(format!(
                    "unknown accepted_by `{other}`; use relais or person"
                ))
            }
        }),
        "failed" => SeedStatus::Failed,
        "blocked" => SeedStatus::Blocked,
        "needs_decision" => SeedStatus::NeedsDecision,
        "needs_review" => SeedStatus::NeedsReview,
        "interrupted" => SeedStatus::Interrupted,
        "budget_exhausted" => SeedStatus::BudgetExhausted,
        "cancelled" => SeedStatus::Cancelled,
        other => {
            return Err(format!(
                "unknown status `{other}`; use accepted, failed, blocked, needs_decision, \
                 needs_review, interrupted, budget_exhausted or cancelled"
            ))
        }
    })
}

fn parse_verdict(text: &str) -> Result<HoldoutVerdict, String> {
    match text {
        "passed" => Ok(HoldoutVerdict::Passed),
        "failed" => Ok(HoldoutVerdict::Failed),
        "did_not_build" => Ok(HoldoutVerdict::DidNotBuild),
        "timed_out" => Ok(HoldoutVerdict::TimedOut),
        "skipped" => Ok(HoldoutVerdict::Skipped),
        other => Err(format!(
            "unknown holdout outcome `{other}`; use passed, failed, did_not_build, timed_out or skipped"
        )),
    }
}

type Fields = serde_json::Map<String, serde_json::Value>;

fn reject_unknown(fields: &Fields, known: &[&str], what: &str) -> Result<(), String> {
    match fields.keys().find(|key| !known.contains(&key.as_str())) {
        Some(key) => Err(format!("unknown field `{key}` in {what}")),
        None => Ok(()),
    }
}

fn text_field<'a>(fields: &'a Fields, key: &str, what: &str) -> Result<Option<&'a str>, String> {
    match fields.get(key) {
        None => Ok(None),
        Some(serde_json::Value::String(text)) if !text.is_empty() => Ok(Some(text)),
        Some(_) => Err(format!("{key} in {what} must be a non-empty string")),
    }
}

fn required_text<'a>(fields: &'a Fields, key: &str, what: &str) -> Result<&'a str, String> {
    text_field(fields, key, what)?.ok_or_else(|| format!("{what} needs {key}"))
}

fn object_list<'a>(fields: &'a Fields, key: &str) -> Result<Vec<&'a Fields>, String> {
    match fields.get(key) {
        None => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_object()
                    .ok_or_else(|| format!("every entry of {key} must be an object"))
            })
            .collect(),
        Some(_) => Err(format!("{key} must be an array")),
    }
}

fn parse_holdouts(fields: &Fields) -> Result<Vec<SeedHoldout>, String> {
    let mut holdouts: Vec<SeedHoldout> = Vec::new();
    for entry in object_list(fields, "holdouts")? {
        reject_unknown(
            entry,
            &["name", "outcome", "profile_green", "source"],
            "a holdout",
        )?;
        let name = required_text(entry, "name", "a holdout")?;
        if holdouts.iter().any(|seen| seen.name == name) {
            return Err(format!("duplicate holdout name `{name}` within the run"));
        }
        let outcome = parse_verdict(required_text(entry, "outcome", "a holdout")?)?;
        let profile_green = match entry.get("profile_green") {
            None => false,
            Some(serde_json::Value::Bool(green)) => *green,
            Some(_) => return Err("profile_green must be true or false".into()),
        };
        let source = match text_field(entry, "source", "a holdout")? {
            None | Some("named") => HoldoutSource::Named,
            Some("attached") => HoldoutSource::Attached,
            Some(other) => return Err(format!("unknown source `{other}`; use named or attached")),
        };
        holdouts.push(SeedHoldout {
            name: name.to_string(),
            outcome,
            profile_green,
            source,
        });
    }
    Ok(holdouts)
}

fn parse_run(fields: &Fields, now: &str) -> Result<SeedRun, String> {
    reject_unknown(
        fields,
        &[
            "run",
            "task",
            "created_at",
            "status",
            "accepted_by",
            "cost_micros",
            "holdouts",
            "exposures",
        ],
        "a run",
    )?;
    let run = required_text(fields, "run", "a run")?;
    let task = required_text(fields, "task", "a run")?;
    let status = parse_status(
        required_text(fields, "status", "a run")?,
        text_field(fields, "accepted_by", "a run")?,
    )?;
    let created_at = match text_field(fields, "created_at", "a run")? {
        None => now.to_string(),
        Some(text) => chrono::DateTime::parse_from_rfc3339(text)
            .map_err(|error| format!("created_at `{text}` is not RFC 3339: {error}"))?
            .with_timezone(&chrono::Utc)
            .to_rfc3339(),
    };
    let cost_micros = match fields.get("cost_micros") {
        None => None,
        Some(cost_field) => match cost_field.as_i64() {
            Some(micros) if micros >= 0 => Some(micros),
            _ => return Err("cost_micros must be a non-negative integer".into()),
        },
    };
    let mut exposures = Vec::new();
    for entry in object_list(fields, "exposures")? {
        reject_unknown(entry, &["marker"], "an exposure")?;
        exposures.push(SeedExposure {
            marker: required_text(entry, "marker", "an exposure")?.to_string(),
        });
    }
    Ok(SeedRun {
        run: run.to_string(),
        task: task.to_string(),
        created_at,
        status,
        cost_micros,
        holdouts: parse_holdouts(fields)?,
        exposures,
    })
}

/// Parse a whole seed file (JSON lines, an optional first line
/// `{"seed_schema": 1}`). Every bad line is returned; nothing is accepted
/// unless all are good. `now` stands in for a missing `created_at`.
pub fn parse_seed(text: &str, now: &str) -> Result<Vec<SeedRun>, Vec<SeedProblem>> {
    let mut runs: Vec<SeedRun> = Vec::new();
    let mut seen_run_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut problems = Vec::new();
    let mut seen_content = false;
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        if raw.trim().is_empty() {
            continue;
        }
        let first = !seen_content;
        seen_content = true;
        let outcome = serde_json::from_str::<serde_json::Value>(raw)
            .map_err(|error| format!("not valid JSON: {error}"))
            .and_then(|document| match document {
                serde_json::Value::Object(fields) => Ok(fields),
                _ => Err("a line must be a JSON object".to_string()),
            })
            .and_then(|fields| {
                // A line refused for another reason still claims its id.
                let already_seen = match fields.get("run") {
                    Some(serde_json::Value::String(id)) if !id.is_empty() => {
                        !seen_run_ids.insert(id.clone())
                    }
                    _ => false,
                };
                if fields.contains_key("seed_schema") {
                    if !first {
                        return Err("seed_schema is only allowed on the first line".into());
                    }
                    reject_unknown(&fields, &["seed_schema"], "the header")?;
                    return match fields["seed_schema"].as_i64() {
                        Some(SEED_SCHEMA) => Ok(None),
                        _ => Err(format!(
                            "unsupported seed_schema {}; this build reads {SEED_SCHEMA}",
                            fields["seed_schema"]
                        )),
                    };
                }
                let parsed = parse_run(&fields, now);
                let run = parsed?;
                if already_seen {
                    return Err(format!("duplicate run id `{}`", run.run));
                }
                Ok(Some(run))
            });
        match outcome {
            Ok(Some(run)) => runs.push(run),
            Ok(None) => {}
            Err(reason) => problems.push(SeedProblem { line, reason }),
        }
    }
    if problems.is_empty() {
        Ok(runs)
    } else {
        Err(problems)
    }
}

/// A canonical form of `path` that does not need `path` to exist: `..`
/// is resolved against the real directory when it exists and lexically
/// when it does not, and the deepest existing ancestor is canonicalised.
fn canonical_even_if_absent(path: &Path) -> io::Result<PathBuf> {
    use std::path::Component;
    let absolute = std::path::absolute(path)?;
    let mut lexical = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                if let Ok(real) = lexical.canonicalize() {
                    lexical = real;
                }
                lexical.pop();
            }
            Component::CurDir => {}
            other => lexical.push(other.as_os_str()),
        }
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = lexical.as_path();
    loop {
        match probe.canonicalize() {
            Ok(mut resolved) => {
                resolved.extend(tail.iter().rev());
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (probe.parent(), probe.file_name()) else {
                    return Err(error);
                };
                tail.push(name.to_os_string());
                probe = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Why the destination guard did not return a state directory.
#[derive(Debug)]
pub enum GuardError {
    /// `RELAIS_STATE_DIR` is unset or empty.
    StateDirNotSet,
    /// The directory is the default one, which holds a person's ledger.
    DefaultStateDir { dir: PathBuf },
    /// A ledger file or sidecar is already there.
    LedgerExists { path: PathBuf },
    /// The filesystem could not answer.
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl std::fmt::Display for GuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StateDirNotSet => write!(f, "RELAIS_STATE_DIR is not set"),
            Self::DefaultStateDir { dir } => {
                write!(f, "{} is the default state directory", dir.display())
            }
            Self::LedgerExists { path } => write!(f, "{} already exists", path.display()),
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "{operation} {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for GuardError {}

/// The checks that run before any ledger is opened. `override_dir` is the
/// value of `RELAIS_STATE_DIR`, `default_dir` the state directory used
/// when it is unset. Returns the state directory to seed.
pub fn guard_destination(
    override_dir: Option<&std::ffi::OsStr>,
    default_dir: Option<&Path>,
) -> Result<PathBuf, GuardError> {
    guard_destination_inspecting(override_dir, default_dir, &|path| {
        std::fs::symlink_metadata(path).map(drop)
    })
}

/// `guard_destination` with the file probe supplied: `inspect` answers
/// whether a path exists without following a final symlink.
fn guard_destination_inspecting(
    override_dir: Option<&std::ffi::OsStr>,
    default_dir: Option<&Path>,
    inspect: &dyn Fn(&Path) -> io::Result<()>,
) -> Result<PathBuf, GuardError> {
    let Some(dir) = override_dir.filter(|dir| !dir.is_empty()) else {
        return Err(GuardError::StateDirNotSet);
    };
    let dir = PathBuf::from(dir);
    let resolve = |path: &Path| {
        canonical_even_if_absent(path).map_err(|source| GuardError::Io {
            operation: "resolve",
            path: path.to_path_buf(),
            source,
        })
    };
    let resolved = resolve(&dir)?;
    if let Some(default_dir) = default_dir {
        if resolve(default_dir)? == resolved {
            return Err(GuardError::DefaultStateDir { dir });
        }
    }
    for name in crate::paths::ledger_file_names() {
        let existing = resolved.join(name);
        match inspect(&existing) {
            Ok(_) => return Err(GuardError::LedgerExists { path: existing }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(GuardError::Io {
                    operation: "inspect",
                    path: existing,
                    source,
                })
            }
        }
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    /// Test-only hooks, thread-local so parallel tests do not see each other.
    pub(super) mod hooks {
        use std::cell::{Cell, RefCell};

        thread_local! {
            static PAUSE_AFTER_FIRST_RUN: RefCell<Option<Box<dyn FnOnce()>>> =
                const { RefCell::new(None) };
            static FAIL_POST_LINK_CLEANUP: Cell<bool> = const { Cell::new(false) };
        }

        /// Run `hook` once, on this thread, when the first run is written.
        pub fn pause_after_first_run(hook: Box<dyn FnOnce()>) {
            PAUSE_AFTER_FIRST_RUN.with(|slot| *slot.borrow_mut() = Some(hook));
        }

        pub fn after_first_run() {
            let hook = PAUSE_AFTER_FIRST_RUN.with(|slot| slot.borrow_mut().take());
            if let Some(hook) = hook {
                hook();
            }
        }

        /// Make removing the staging name after the link fail, once.
        pub fn fail_post_link_cleanup() {
            FAIL_POST_LINK_CLEANUP.with(|flag| flag.set(true));
        }

        pub fn take_cleanup_failure() -> bool {
            FAIL_POST_LINK_CLEANUP.with(|flag| flag.replace(false))
        }
    }

    use super::*;
    use crate::storage_full::failpoint::{arm, Armed, Fire, Script};
    use crate::storage_full::Site;
    use crate::test_support::short_temp_dir;
    use rusqlite::Connection;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc;

    fn run(id: &str, created_at: &str, status: SeedStatus) -> SeedRun {
        SeedRun {
            run: id.into(),
            task: format!("task-{id}"),
            created_at: created_at.into(),
            status,
            cost_micros: None,
            holdouts: Vec::new(),
            exposures: Vec::new(),
        }
    }

    fn three_runs() -> Vec<SeedRun> {
        let mut relais = run(
            "r1",
            "2026-10-01T00:00:00+00:00",
            SeedStatus::Accepted(AcceptedBy::Relais),
        );
        relais.cost_micros = Some(120_000);
        relais.exposures = vec![SeedExposure {
            marker: "store".into(),
        }];
        let person = run(
            "r2",
            "2026-10-02T00:00:00+00:00",
            SeedStatus::Accepted(AcceptedBy::Person),
        );
        let mut waiting = run("r3", "2026-10-03T00:00:00+00:00", SeedStatus::NeedsDecision);
        waiting.holdouts = vec![
            SeedHoldout {
                name: "report-json".into(),
                outcome: HoldoutVerdict::Failed,
                profile_green: true,
                source: HoldoutSource::Named,
            },
            SeedHoldout {
                name: "attached-one".into(),
                outcome: HoldoutVerdict::Skipped,
                profile_green: false,
                source: HoldoutSource::Attached,
            },
        ];
        vec![relais, person, waiting]
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    fn query<T>(
        dir: &Path,
        sql: &str,
        map: impl Fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Vec<T> {
        let conn = Connection::open_with_flags(
            dir.join(LEDGER_FILE),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("open");
        let mut statement = conn.prepare(sql).expect("prepare");
        let rows = statement.query_map([], |row| map(row)).expect("query");
        rows.collect::<Result<_, _>>().expect("rows")
    }

    fn run_rows(dir: &Path) -> Vec<(String, String, String)> {
        query(
            dir,
            "SELECT id, created_at, status FROM runs ORDER BY id",
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    }

    #[test]
    fn ledger_close_checkpoints_and_leaves_no_wal_sidecar() {
        let dir = short_temp_dir("ledger-close");
        let path = dir.join(LEDGER_FILE);
        let wal = dir.join(format!("{LEDGER_FILE}{WAL_SUFFIX}"));
        let ledger = Ledger::open(&path).expect("open");
        ledger
            .insert_run(
                &RunId::from_stored("r1"),
                "/repo",
                None,
                &TaskId::from_stored("t1"),
                "rk",
            )
            .expect("insert");
        assert!(wal.exists(), "an open WAL ledger has a -wal file");
        ledger.close().expect("close");
        assert!(!wal.exists(), "close leaves no -wal file");
    }

    #[test]
    fn three_runs_publish_a_complete_ledger_and_no_staging_file() {
        let dir = short_temp_dir("seed-three");
        let outcome = write_seed(&dir, &three_runs()).expect("seed");
        assert_eq!(
            outcome,
            SeedOutcome::Published {
                path: dir.join(LEDGER_FILE),
                cleanup_warning: None
            }
        );
        assert_eq!(file_names(&dir), vec![LEDGER_FILE.to_string()]);
        assert!(
            !dir.join(format!("{LEDGER_FILE}{WAL_SUFFIX}")).exists(),
            "no -wal sits beside the ledger"
        );
        assert_eq!(
            run_rows(&dir),
            vec![
                (
                    "r1".into(),
                    "2026-10-01T00:00:00+00:00".into(),
                    "accepted".into()
                ),
                (
                    "r2".into(),
                    "2026-10-02T00:00:00+00:00".into(),
                    "accepted".into()
                ),
                (
                    "r3".into(),
                    "2026-10-03T00:00:00+00:00".into(),
                    "needs_decision".into()
                ),
            ]
        );
        let reasons: Vec<(String, String)> = query(
            &dir,
            "SELECT run_id, reason FROM transitions WHERE to_state = 'accepted' ORDER BY run_id",
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        assert_eq!(
            reasons,
            vec![
                ("r1".to_string(), "checks_and_review_passed".to_string()),
                ("r2".to_string(), "decision_approved".to_string()),
            ]
        );
        let holdouts: Vec<(String, String, String, i64)> = query(
            &dir,
            "SELECT run_id, name, outcome, attempt_index FROM holdout_outcomes ORDER BY name",
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        );
        assert_eq!(
            holdouts,
            vec![
                ("r3".into(), "attached-one".into(), "skipped".into(), 1),
                ("r3".into(), "report-json".into(), "failed".into(), 1),
            ]
        );
        let exposures: Vec<String> = query(&dir, "SELECT marker FROM holdout_exposures", |row| {
            row.get(0)
        });
        assert_eq!(exposures, vec!["store".to_string()]);
        let costs: Vec<i64> = query(&dir, "SELECT cost_micros FROM usage_events", |row| {
            row.get(0)
        });
        assert_eq!(costs, vec![120_000]);
        let open_decisions: Vec<String> = query(
            &dir,
            "SELECT run FROM decisions WHERE resolved_at IS NULL",
            |row| row.get(0),
        );
        assert_eq!(open_decisions, vec!["r3".to_string()]);
        let integrity: Vec<String> = query(&dir, "PRAGMA integrity_check", |row| row.get(0));
        assert_eq!(integrity, vec!["ok".to_string()]);
    }

    fn arm_storage_full_after_first_run() -> Rc<RefCell<Option<Armed>>> {
        let slot = Rc::new(RefCell::new(None));
        let hook_slot = Rc::clone(&slot);
        hooks::pause_after_first_run(Box::new(move || {
            *hook_slot.borrow_mut() = Some(arm(Script {
                fires: vec![Fire {
                    site: Site::Ledger,
                    after: None,
                }],
                ..Script::default()
            }));
        }));
        slot
    }

    #[test]
    fn storage_full_at_run_two_leaves_nothing() {
        let dir = short_temp_dir("seed-full");
        let _armed = arm_storage_full_after_first_run();
        let error = write_seed(&dir, &three_runs()).expect_err("storage full");
        match error {
            SeedError::Ledger(ledger_error) => assert!(ledger_error.is_storage_full()),
            other => panic!("expected a ledger error, got {other}"),
        }
        assert_eq!(file_names(&dir), Vec::<String>::new());
    }

    #[test]
    fn a_panic_at_run_two_unwinds_through_the_guard() {
        let dir = short_temp_dir("seed-panic");
        hooks::pause_after_first_run(Box::new(|| panic!("injected panic")));
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            write_seed(&dir, &three_runs())
        }));
        assert!(unwound.is_err(), "the panic propagates");
        assert_eq!(file_names(&dir), Vec::<String>::new());
    }

    #[test]
    fn two_writers_exactly_one_publishes_and_neither_removes_the_others_staging() {
        let dir = short_temp_dir("seed-race");
        let loser_dir = dir.to_path_buf();
        let (paused_tx, paused_rx) = mpsc::channel::<()>();
        let (resume_tx, resume_rx) = mpsc::channel::<()>();
        let loser_runs = vec![
            run("loser-1", "2026-10-01T00:00:00+00:00", SeedStatus::Failed),
            run("loser-2", "2026-10-02T00:00:00+00:00", SeedStatus::Failed),
        ];
        let paused_writer = std::thread::spawn(move || {
            hooks::pause_after_first_run(Box::new(move || {
                paused_tx.send(()).expect("signal paused");
                resume_rx.recv().expect("resume");
            }));
            write_seed(&loser_dir, &loser_runs)
        });
        paused_rx.recv().expect("the first writer paused");
        let paused_staging = file_names(&dir);
        assert_eq!(paused_staging.len(), 1, "the paused writer's staging file");

        let winner_runs = vec![run(
            "winner-1",
            "2026-10-05T00:00:00+00:00",
            SeedStatus::Blocked,
        )];
        assert_eq!(
            write_seed(&dir, &winner_runs).expect("winner"),
            SeedOutcome::Published {
                path: dir.join(LEDGER_FILE),
                cleanup_warning: None
            }
        );
        let mut expected = paused_staging.clone();
        expected.push(LEDGER_FILE.to_string());
        expected.sort();
        assert_eq!(
            file_names(&dir),
            expected,
            "the winner kept the paused writer's staging file"
        );

        resume_tx.send(()).expect("resume");
        let loser = paused_writer.join().expect("join").expect("loser result");
        assert_eq!(
            loser,
            SeedOutcome::DestinationExists {
                path: dir.join(LEDGER_FILE)
            }
        );
        assert_eq!(file_names(&dir), vec![LEDGER_FILE.to_string()]);
        let ids: Vec<String> = run_rows(&dir).into_iter().map(|row| row.0).collect();
        assert_eq!(ids, vec!["winner-1".to_string()]);
    }

    #[test]
    fn a_failed_cleanup_after_the_link_is_a_warning_and_the_ledger_is_complete() {
        let dir = short_temp_dir("seed-cleanup");
        hooks::fail_post_link_cleanup();
        let outcome = write_seed(&dir, &three_runs()).expect("seed");
        match outcome {
            SeedOutcome::Published {
                cleanup_warning: Some(warning),
                ..
            } => assert!(warning.contains("staging file"), "{warning}"),
            other => panic!("expected a published outcome with a warning, got {other:?}"),
        }
        assert_eq!(run_rows(&dir).len(), 3);
        assert!(dir.join(LEDGER_FILE).exists());
    }

    #[test]
    fn a_later_valid_line_repeating_a_refused_lines_run_id_is_a_duplicate() {
        let first = r#"{"run":"r1","task":"t","status":"sleeping"}"#;
        let second = r#"{"run":"r1","task":"t","status":"failed"}"#;
        let problems = parse_seed(&format!("{first}\n{second}"), "2026-10-01T00:00:00+00:00")
            .expect_err("both lines are refused");
        assert_eq!(problems.len(), 2);
        assert!(
            problems[0].reason.contains("unknown status"),
            "{problems:?}"
        );
        assert_eq!(problems[1].line, 2);
        assert_eq!(problems[1].reason, "duplicate run id `r1`");
    }

    #[test]
    fn a_header_marked_line_after_the_first_still_claims_its_run_id() {
        let first = r#"{"run":"r0","task":"t","status":"failed"}"#;
        let header = r#"{"seed_schema":1,"run":"r1"}"#;
        let later = r#"{"run":"r1","task":"t","status":"failed"}"#;
        let problems = parse_seed(
            &format!("{first}\n{header}\n{later}"),
            "2026-10-01T00:00:00+00:00",
        )
        .expect_err("two lines are refused");
        assert_eq!(problems.len(), 2);
        assert_eq!(problems[0].line, 2);
        assert_eq!(problems[1].line, 3);
        assert_eq!(problems[1].reason, "duplicate run id `r1`");
    }

    #[test]
    fn a_metadata_error_other_than_not_found_is_an_io_failure() {
        let dir = short_temp_dir("guard-io");
        let error = guard_destination_inspecting(Some(dir.as_os_str()), None, &|_| {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .expect_err("cannot be inspected");
        match error {
            GuardError::Io {
                operation, source, ..
            } => {
                assert_eq!(operation, "inspect");
                assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
            }
            other => panic!("expected an inspect failure, got {other}"),
        }
    }

    #[test]
    fn existing_ledger_files_and_unset_dirs_are_refusals() {
        let dir = short_temp_dir("guard-refuse");
        assert!(matches!(
            guard_destination(None, None),
            Err(GuardError::StateDirNotSet)
        ));
        std::fs::write(dir.join(format!("{LEDGER_FILE}{SHM_SUFFIX}")), b"").expect("shm");
        assert!(matches!(
            guard_destination(Some(dir.as_os_str()), None),
            Err(GuardError::LedgerExists { .. })
        ));
    }
}
