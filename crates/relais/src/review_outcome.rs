//! Plan-review outcomes (plan-review-learns-model, Behaviour 2 and 6):
//! amont-agent's `plan-review record` ledger read, each reviewer labelled
//! `clean`, `miss` or `unknown`, and the labels written to the relais
//! ledger's `review_outcomes` table.
//!
//! The ledger is another tool's file under the person's home, so it is read
//! behind a trust boundary (never a symlink, owned by us, no group or other
//! access) and every row is validated. Labelling is a pure function of the
//! rows and two lookups (is the plan pushed, what is the body with this
//! sha), which is what the tests drive; the reading and the writing sit
//! around it.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ledger::{Ledger, LedgerError, ReviewOutcomeRow};
use crate::paths;

/// The only `schema` value a row may carry.
const ROW_SCHEMA: &str = "amont-agent-review-outcome-v1";
/// Where amont-agent keeps its ledger, under the Claude config directory.
const LEDGER_PARTS: [&str; 3] = ["amont-agent", "plan-review", "outcomes.jsonl"];
/// The environment variable amont-agent resolves its plan root from.
pub const PLAN_ROOT_ENV: &str = "AMONT_AGENT_PLAN_ROOT";
/// How much of a body sha a plan file's `body-sha=` stamp carries.
const STAMP_LEN: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Blocking,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Disposition {
    Accepted,
    Rejected,
    Decision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum Verdict {
    #[serde(rename = "rework")]
    Rework,
    #[serde(rename = "approve-with-changes")]
    ApproveWithChanges,
    #[serde(rename = "approve")]
    Approve,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rework => "rework",
            Self::ApproveWithChanges => "approve-with-changes",
            Self::Approve => "approve",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    // Validated for shape (`deny_unknown_fields` needs every key declared),
    // not read by the labels.
    #[serde(rename = "role")]
    _role: String,
    severity: Severity,
    #[serde(rename = "disposition")]
    _disposition: Disposition,
    #[serde(default)]
    section: Option<String>,
    #[serde(default)]
    area: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanFeatures {
    // Validated, not read yet: Phase 8 strata may split on them.
    #[serde(rename = "areas")]
    _areas: Vec<String>,
    repos: Vec<String>,
    #[serde(rename = "body_lines")]
    _body_lines: u64,
}

/// One line of the ledger.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    schema: String,
    recorded_at: i64,
    role: String,
    agent_id: String,
    model: Option<String>,
    tokens: Option<u64>,
    seconds: Option<f64>,
    round: u32,
    body_sha: String,
    first_round_sha: String,
    plan_path: String,
    /// Null when amont-agent never saved the body this reviewer read.
    plan: Option<PlanFeatures>,
    verdict: Option<Verdict>,
    remit: Vec<String>,
    findings: Vec<Finding>,
}

/// Why a review could not be called either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    NotPushed,
    NoLaterReview,
    MissingBody,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotPushed => "not_pushed",
            Self::NoLaterReview => "no_later_review",
            Self::MissingBody => "missing_body",
        }
    }
}

/// A review's label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    Clean,
    Miss,
    Unknown(Reason),
}

impl Label {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Miss => "miss",
            Self::Unknown(_) => "unknown",
        }
    }

    fn reason(self) -> Option<Reason> {
        match self {
            Self::Unknown(reason) => Some(reason),
            _ => None,
        }
    }
}

/// Everything labelling needs from the outside world.
pub struct Sources<'a> {
    /// Whether a plan file in any of these repos carries any of these
    /// `body-sha=` stamps.
    pub pushed: &'a dyn Fn(&[String], &[String]) -> bool,
    /// The saved plan body of a sha, when it is there and trusted.
    pub body: &'a dyn Fn(&str) -> Option<String>,
}

/// The text under the heading named `name`, from that heading line up to
/// the next heading of the same or a higher level; any level matches the
/// name. Headings inside a fenced code block are text.
fn section_text<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let mut fence: Option<char> = None;
    let mut offset = 0;
    let mut open: Option<(usize, usize)> = None;
    for line in body.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim_start();
        if let Some(marker) = trimmed.chars().next().filter(|c| matches!(c, '`' | '~')) {
            if trimmed.starts_with(&marker.to_string().repeat(3)) {
                fence = match fence {
                    None => Some(marker),
                    Some(open_marker) if open_marker == marker => None,
                    other => other,
                };
                continue;
            }
        }
        if fence.is_some() {
            continue;
        }
        let Some((level, title)) = heading(line) else {
            continue;
        };
        if let Some((from, opened_at)) = open {
            if level <= opened_at {
                return Some(&body[from..start]);
            }
        } else if title == name {
            open = Some((start, level));
        }
    }
    open.map(|(from, _)| &body[from..])
}

/// A markdown heading line's level and title, or `None`.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t', '\r', '\n']) {
        return None;
    }
    Some((level, rest.trim()))
}

/// Label one review against the later backend rounds of its plan.
fn label_row(
    row: &Row,
    plan: &[&Row],
    pushed: bool,
    body: &dyn Fn(&str) -> Option<String>,
) -> Label {
    if !pushed {
        return Label::Unknown(Reason::NotPushed);
    }
    let later: Vec<&&Row> = plan
        .iter()
        .filter(|other| other.role == "backend" && other.round > row.round)
        .collect();
    if later.is_empty() {
        return Label::Unknown(Reason::NoLaterReview);
    }
    let everything = row.remit.iter().any(|area| area == "*");
    let in_remit = |finding: &Finding| {
        everything
            || finding
                .area
                .as_ref()
                .is_some_and(|area| row.remit.contains(area))
    };
    let mut undecidable = false;
    for later_row in later {
        for finding in &later_row.findings {
            let Some(section) = finding.section.as_deref().filter(|s| !s.is_empty()) else {
                continue;
            };
            if !matches!(finding.severity, Severity::High | Severity::Blocking)
                || !in_remit(finding)
            {
                continue;
            }
            let (Some(first), Some(last)) = (body(&row.first_round_sha), body(&later_row.body_sha))
            else {
                undecidable = true;
                continue;
            };
            // Only a section that stood unchanged in round 1 was there to
            // be found by this reviewer.
            match (section_text(&first, section), section_text(&last, section)) {
                (Some(then), Some(now)) if then == now => {}
                _ => continue,
            }
            if !row
                .findings
                .iter()
                .any(|own| own.section.as_deref() == Some(section))
            {
                return Label::Miss;
            }
        }
    }
    if undecidable {
        Label::Unknown(Reason::MissingBody)
    } else {
        Label::Clean
    }
}

/// Every row's label, in row order. Rows are grouped by plan path.
fn label_rows(rows: &[Row], sources: &Sources<'_>) -> Vec<Label> {
    let mut plans: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        plans.entry(row.plan_path.as_str()).or_default().push(row);
    }
    // A plan's repositories, from every row that recorded them: a row
    // whose body was never saved carries none of its own.
    let repos: BTreeMap<&str, Vec<String>> = plans
        .iter()
        .map(|(path, rows)| {
            let set: BTreeSet<&String> = rows
                .iter()
                .filter_map(|r| r.plan.as_ref())
                .flat_map(|p| &p.repos)
                .collect();
            (*path, set.into_iter().cloned().collect())
        })
        .collect();
    let mut pushed_cache: HashMap<&str, bool> = HashMap::new();
    rows.iter()
        .map(|row| {
            let plan = &plans[row.plan_path.as_str()];
            let pushed = *pushed_cache
                .entry(row.plan_path.as_str())
                .or_insert_with(|| {
                    let stamps: Vec<String> = plan
                        .iter()
                        .map(|r| r.body_sha.chars().take(STAMP_LEN).collect::<String>())
                        .filter(|stamp| !stamp.is_empty())
                        .map(|stamp| format!("body-sha={stamp}"))
                        .collect();
                    (sources.pushed)(&repos[row.plan_path.as_str()], &stamps)
                });
            label_row(row, plan, pushed, sources.body)
        })
        .collect()
}

/// Where the ledger and the plans are looked for.
#[derive(Debug, Clone)]
pub struct Settings {
    pub ledger: PathBuf,
    pub plan_root: PathBuf,
}

impl Settings {
    /// The flags when given, otherwise amont-agent's own defaults.
    pub fn resolve(
        ledger: Option<PathBuf>,
        plan_root: Option<PathBuf>,
    ) -> Result<Self, paths::HomeUnset> {
        let ledger = match ledger {
            Some(path) => path,
            None => {
                let base = match paths::dir_override(paths::CLAUDE_CONFIG_DIR_ENV) {
                    Some(dir) => dir,
                    None => paths::home_dir()?.join(".claude"),
                };
                LEDGER_PARTS.iter().fold(base, |dir, part| dir.join(part))
            }
        };
        let plan_root = match plan_root.or_else(|| paths::dir_override(PLAN_ROOT_ENV)) {
            Some(dir) => dir,
            None => paths::home_dir()?.join("Developer").join("Perso"),
        };
        Ok(Self { ledger, plan_root })
    }
}

/// Why a read failed; the exit code follows the variant.
#[derive(Debug)]
pub enum IngestError {
    /// The ledger file crosses the trust boundary (exit 3).
    Refused(String),
    /// The ledger file cannot be read (exit 2).
    Unreadable(String),
    /// The relais ledger failed (exit 1).
    Ledger(LedgerError),
}

impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(why) | Self::Unreadable(why) => f.write_str(why),
            Self::Ledger(e) => write!(f, "the relais ledger: {e}"),
        }
    }
}

impl std::error::Error for IngestError {}

impl From<LedgerError> for IngestError {
    fn from(e: LedgerError) -> Self {
        Self::Ledger(e)
    }
}

/// What a read of the ledger found.
#[derive(Debug, Clone, PartialEq)]
pub struct Labelled {
    pub rows: Vec<ReviewOutcomeRow>,
    /// Lines that did not parse, or carried another schema.
    pub invalid: usize,
    reasons: Vec<Option<Reason>>,
}

/// The state of a file as the trust boundary sees it.
enum FileState {
    Absent,
    Trusted,
    Refused(String),
    NotAFile,
}

fn file_state(path: &Path) -> std::io::Result<FileState> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileState::Absent),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() {
        return Ok(FileState::Refused("is a symlink".into()));
    }
    if !meta.is_file() {
        return Ok(FileState::NotAFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != crate::procs::uid() {
            return Ok(FileState::Refused(
                "is not owned by the running user".into(),
            ));
        }
        if meta.mode() & 0o077 != 0 {
            return Ok(FileState::Refused(format!(
                "has mode {:04o}; group and other access must be off",
                meta.mode() & 0o777
            )));
        }
    }
    Ok(FileState::Trusted)
}

/// A saved body, when its sha is plain hex and the file passes the same
/// trust check as the ledger; a refused or unreadable body is a missing one.
fn read_body(dir: &Path, sha: &str) -> Option<String> {
    if sha.is_empty() || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let path = dir.join("bodies").join(format!("{sha}.md"));
    match file_state(&path) {
        Ok(FileState::Trusted) => std::fs::read_to_string(path).ok(),
        _ => None,
    }
}

/// Whether any `<root>/<repo>/docs/plans/*.md` carries one of the stamps.
fn plan_pushed(root: &Path, repos: &[String], stamps: &[String]) -> bool {
    repos
        .iter()
        .filter(|repo| !repo.is_empty() && !repo.contains(['/', '\\']) && *repo != "..")
        .any(|repo| {
            let Ok(entries) = std::fs::read_dir(root.join(repo).join("docs").join("plans")) else {
                return false;
            };
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
                .filter_map(|path| std::fs::read_to_string(path).ok())
                .any(|text| stamps.iter().any(|stamp| text.contains(stamp.as_str())))
        })
}

/// Read and label the ledger. `None` when there is no ledger file.
pub fn read(settings: &Settings) -> Result<Option<Labelled>, IngestError> {
    let unreadable = |e: &dyn fmt::Display| {
        IngestError::Unreadable(format!("{}: {e}", settings.ledger.display()))
    };
    match file_state(&settings.ledger).map_err(|e| unreadable(&e))? {
        FileState::Absent => return Ok(None),
        FileState::Refused(why) => {
            return Err(IngestError::Refused(format!(
                "{} {why}",
                settings.ledger.display()
            )))
        }
        FileState::NotAFile => return Err(unreadable(&"not a regular file")),
        FileState::Trusted => {}
    }
    let bytes = std::fs::read(&settings.ledger).map_err(|e| unreadable(&e))?;
    let mut invalid = 0;
    let mut rows: Vec<Row> = Vec::new();
    for line in bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.trim_ascii().is_empty())
    {
        match serde_json::from_slice::<Row>(line) {
            Ok(row) if row.schema == ROW_SCHEMA => {
                // A line recorded twice is one review: the first stands.
                if !rows.iter().any(|seen| seen.agent_id == row.agent_id) {
                    rows.push(row);
                }
            }
            _ => invalid += 1,
        }
    }
    let dir = settings.ledger.parent().unwrap_or(Path::new("."));
    let bodies: RefCell<HashMap<String, Option<String>>> = RefCell::new(HashMap::new());
    let body = |sha: &str| -> Option<String> {
        bodies
            .borrow_mut()
            .entry(sha.to_string())
            .or_insert_with(|| read_body(dir, sha))
            .clone()
    };
    let pushed =
        |repos: &[String], stamps: &[String]| plan_pushed(&settings.plan_root, repos, stamps);
    let labels = label_rows(
        &rows,
        &Sources {
            pushed: &pushed,
            body: &body,
        },
    );
    let reasons = labels.iter().map(|label| label.reason()).collect();
    let rows = rows
        .into_iter()
        .zip(labels)
        .map(|(row, label)| ReviewOutcomeRow {
            agent_id: row.agent_id,
            plan_path: row.plan_path,
            class: row.role.clone(),
            role: row.role,
            round: row.round,
            body_sha: row.body_sha,
            model: row.model,
            tokens: row.tokens,
            seconds: row.seconds,
            verdict: row.verdict.map(|v| v.as_str().to_string()),
            label: label.as_str().to_string(),
            label_reason: label.reason().map(|r| r.as_str().to_string()),
            router_task_id: None,
            recorded_at: row.recorded_at,
        })
        .collect();
    Ok(Some(Labelled {
        rows,
        invalid,
        reasons,
    }))
}

impl Labelled {
    fn count(&self, label: &str) -> usize {
        self.rows.iter().filter(|row| row.label == label).count()
    }

    /// `labelled N: clean a, miss b, unknown c`, with the invalid lines and
    /// the unknowns' reasons when there are any.
    pub fn summary(&self) -> String {
        let unknown = self.count("unknown");
        let mut line = format!(
            "labelled {}: clean {}, miss {}, unknown {}",
            self.rows.len(),
            self.count("clean"),
            self.count("miss"),
            unknown
        );
        if self.invalid > 0 {
            line.push_str(&format!(", invalid {}", self.invalid));
        }
        if unknown > 0 {
            let of = |reason: Reason| {
                self.reasons
                    .iter()
                    .flatten()
                    .filter(|r| **r == reason)
                    .count()
            };
            line.push_str(&format!(
                " (not_pushed {}, no_later_review {}, missing_body {})",
                of(Reason::NotPushed),
                of(Reason::NoLaterReview),
                of(Reason::MissingBody)
            ));
        }
        line
    }

    /// One line per review, as `--dry-run` prints them.
    pub fn lines(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                let reason = row
                    .label_reason
                    .as_ref()
                    .map(|reason| format!(" {reason}"))
                    .unwrap_or_default();
                format!(
                    "{} {} round {} {}{reason}",
                    row.agent_id, row.role, row.round, row.label
                )
            })
            .collect()
    }

    /// Join each review to its router task and write them all.
    pub fn record(&self, ledger: &Ledger) -> Result<(), LedgerError> {
        let mut rows = self.rows.clone();
        for row in &mut rows {
            row.router_task_id = ledger.router_task_for_agent(&row.agent_id)?;
        }
        ledger.record_review_outcomes(&rows)
    }
}

/// What the learn cycle notes: the ingest with default settings, or the
/// reason it did not happen. `None` when there is no ledger to read.
pub fn cycle_note(ledger: &Ledger) -> Option<String> {
    let outcome = Settings::resolve(None, None)
        .map_err(|e| e.to_string())
        .and_then(|settings| {
            let labelled = read(&settings).map_err(|e| e.to_string())?;
            let Some(labelled) = labelled else {
                return Ok(None);
            };
            labelled.record(ledger).map_err(|e| e.to_string())?;
            Ok(Some(labelled.summary()))
        });
    match outcome {
        Ok(summary) => summary.map(|summary| format!("review outcomes {summary}")),
        Err(e) => Some(format!("review outcomes: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY1: &str = "# Plan\n\n## Behaviour\n\nOne.\n\n## Phases\n\nTwo.\n";
    const BODY2_SAME: &str = "# Plan\n\n## Behaviour\n\nOne.\n\n## Phases\n\nTwo changed.\n";

    fn row(agent: &str, role: &str, round: u32, body_sha: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": ROW_SCHEMA, "recorded_at": 10, "role": role, "agent_id": agent,
            "model": "claude-fable-5-1", "tokens": 1000, "seconds": 2.5, "round": round,
            "body_sha": body_sha, "first_round_sha": "aaaaaaaaaaaaaaaa",
            "plan_path": "/plans/p.md",
            "plan": {"areas": ["cli"], "repos": ["relais"], "body_lines": 9},
            "verdict": "approve", "remit": if role == "backend" { vec!["*"] } else { vec!["cli"] },
            "findings": [],
        })
    }

    fn with_finding(
        mut value: serde_json::Value,
        section: &str,
        area: &str,
        sev: &str,
    ) -> serde_json::Value {
        let role = value["role"].clone();
        value["findings"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "role": role, "severity": sev, "disposition": "accepted",
                "section": section, "area": area,
            }));
        value
    }

    fn parse(values: &[serde_json::Value]) -> Vec<Row> {
        values
            .iter()
            .map(|v| serde_json::from_value(v.clone()).expect("row"))
            .collect()
    }

    fn labels(values: &[serde_json::Value], pushed: bool, bodies: &[(&str, &str)]) -> Vec<Label> {
        let bodies: HashMap<String, String> = bodies
            .iter()
            .map(|(sha, text)| (sha.to_string(), text.to_string()))
            .collect();
        let push = move |_: &[String], _: &[String]| pushed;
        let body = |sha: &str| bodies.get(sha).cloned();
        label_rows(
            &parse(values),
            &Sources {
                pushed: &push,
                body: &body,
            },
        )
    }

    const FIRST: (&str, &str) = ("aaaaaaaaaaaaaaaa", BODY1);

    #[test]
    fn a_plan_never_pushed_is_unknown() {
        let rows = [row("r", "rust", 1, "b1"), row("b", "backend", 2, "b2")];
        assert_eq!(
            labels(&rows, false, &[])[0],
            Label::Unknown(Reason::NotPushed)
        );
    }

    #[test]
    fn a_review_with_no_later_backend_round_is_unknown() {
        let rows = [row("r", "rust", 1, "b1"), row("r2", "rust", 2, "b2")];
        assert_eq!(
            labels(&rows, true, &[])[0],
            Label::Unknown(Reason::NoLaterReview)
        );
    }

    #[test]
    fn a_later_high_finding_in_remit_on_an_unchanged_section_is_a_miss() {
        let rows = [
            row("r", "rust", 1, "b1"),
            with_finding(row("b", "backend", 2, "b2"), "Behaviour", "cli", "high"),
        ];
        let out = labels(&rows, true, &[FIRST, ("b2", BODY2_SAME)]);
        assert_eq!(out[0], Label::Miss);
        // The backend itself raised nothing before round 2: it is round 2.
        assert_eq!(out[1], Label::Unknown(Reason::NoLaterReview));
    }

    #[test]
    fn a_role_that_raised_the_section_itself_is_clean() {
        let rows = [
            with_finding(row("r", "rust", 1, "b1"), "Behaviour", "cli", "low"),
            with_finding(row("b", "backend", 2, "b2"), "Behaviour", "cli", "blocking"),
        ];
        assert_eq!(
            labels(&rows, true, &[FIRST, ("b2", BODY2_SAME)])[0],
            Label::Clean
        );
    }

    #[test]
    fn a_section_that_changed_since_round_one_does_not_count() {
        let rows = [
            row("r", "rust", 1, "b1"),
            with_finding(row("b", "backend", 2, "b2"), "Phases", "cli", "high"),
        ];
        assert_eq!(
            labels(&rows, true, &[FIRST, ("b2", BODY2_SAME)])[0],
            Label::Clean
        );
    }

    #[test]
    fn a_finding_outside_the_remit_or_below_high_does_not_count() {
        let rows = [
            row("r", "rust", 1, "b1"),
            with_finding(row("b", "backend", 2, "b2"), "Behaviour", "ui", "high"),
            with_finding(row("c", "backend", 3, "b3"), "Behaviour", "cli", "medium"),
        ];
        let out = labels(
            &rows,
            true,
            &[FIRST, ("b2", BODY2_SAME), ("b3", BODY2_SAME)],
        );
        assert_eq!(out[0], Label::Clean);
    }

    #[test]
    fn a_missing_body_makes_a_candidate_undecidable() {
        let rows = [
            row("r", "rust", 1, "b1"),
            with_finding(row("b", "backend", 2, "b2"), "Behaviour", "cli", "high"),
        ];
        assert_eq!(
            labels(&rows, true, &[("b2", BODY2_SAME)])[0],
            Label::Unknown(Reason::MissingBody)
        );
    }

    #[test]
    fn a_miss_outranks_an_undecidable_finding() {
        let rows = [
            row("r", "rust", 1, "b1"),
            with_finding(row("b", "backend", 2, "b2"), "Behaviour", "cli", "high"),
            with_finding(row("c", "backend", 3, "b3"), "Behaviour", "cli", "high"),
        ];
        // b2's body is missing, b3's is there: b3's finding decides it.
        let out = labels(&rows, true, &[FIRST, ("b3", BODY2_SAME)]);
        assert_eq!(out[0], Label::Miss);
        let out = labels(&rows, true, &[FIRST, ("b2", BODY2_SAME)]);
        assert_eq!(out[0], Label::Miss);
        // Neither later body: nothing decides it.
        let out = labels(&rows, true, &[FIRST]);
        assert_eq!(out[0], Label::Unknown(Reason::MissingBody));
    }

    #[test]
    fn sections_run_to_the_next_heading_of_the_same_or_higher_level() {
        let body = "# T\n\n## A\n\nx\n\n### Sub\n\ny\n\n## B\n\nz\n";
        assert_eq!(
            section_text(body, "A"),
            Some("## A\n\nx\n\n### Sub\n\ny\n\n")
        );
        assert_eq!(section_text(body, "Sub"), Some("### Sub\n\ny\n\n"));
        assert_eq!(section_text(body, "B"), Some("## B\n\nz\n"));
        assert_eq!(section_text(body, "Nope"), None);
        let fenced = "## A\n\n```\n## B\n```\n\n## C\n";
        assert_eq!(
            section_text(fenced, "A"),
            Some("## A\n\n```\n## B\n```\n\n")
        );
        assert_eq!(section_text(fenced, "B"), None);
    }

    #[test]
    fn dry_run_lines_and_summary_name_the_reasons() {
        let rows = [row("r", "rust", 1, "b1")];
        let parsed = parse(&rows);
        let out = labels(&rows, false, &[]);
        let labelled = Labelled {
            rows: parsed
                .iter()
                .zip(&out)
                .map(|(row, label)| ReviewOutcomeRow {
                    agent_id: row.agent_id.clone(),
                    plan_path: row.plan_path.clone(),
                    role: row.role.clone(),
                    class: row.role.clone(),
                    round: row.round,
                    body_sha: row.body_sha.clone(),
                    model: None,
                    tokens: None,
                    seconds: None,
                    verdict: None,
                    label: label.as_str().into(),
                    label_reason: label.reason().map(|r| r.as_str().into()),
                    router_task_id: None,
                    recorded_at: 0,
                })
                .collect(),
            invalid: 2,
            reasons: out.iter().map(|l| l.reason()).collect(),
        };
        assert_eq!(labelled.lines(), vec!["r rust round 1 unknown not_pushed"]);
        assert_eq!(
            labelled.summary(),
            "labelled 1: clean 0, miss 0, unknown 1, invalid 2 \
             (not_pushed 1, no_later_review 0, missing_body 0)"
        );
    }

    #[test]
    fn rows_with_unknown_fields_or_another_schema_do_not_parse_as_reviews() {
        let mut extra = row("r", "rust", 1, "b1");
        extra["surprise"] = serde_json::json!(1);
        assert!(serde_json::from_value::<Row>(extra).is_err());
        let mut other = row("r", "rust", 1, "b1");
        other["schema"] = serde_json::json!("v2");
        let parsed: Row = serde_json::from_value(other).expect("shape parses");
        assert_ne!(parsed.schema, ROW_SCHEMA);
    }

    #[test]
    fn a_body_sha_that_is_not_hex_is_never_a_path() {
        let dir = crate::test_support::temp_dir("review-body");
        assert_eq!(read_body(&dir, "../x"), None);
        assert_eq!(read_body(&dir, ""), None);
    }
}
