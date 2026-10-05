//! What a sandboxed worker attempt was denied, and how complete that
//! account is (SPEC §8, "Denial evidence").
//!
//! [`scan`] reads the attempt's Claude Code transcript and the files the
//! worker left in its scratch directory. The only denial the measured
//! harness reports in structure is a network one, a `deny network-outbound
//! <host>:<port>` line inside a `<sandbox_violations>` block of a
//! `tool_result`: that is VERIFIED. Any other line that reads like an OS
//! or proxy refusal is only SUSPECTED, because an ordinary application
//! error can carry the same words. [`Coverage`] says whether the transcript
//! was whole, so an empty report is never mistaken for a clean one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::probe::{clip, OS_DENIALS, PERMISSION_REFUSALS};

const VIOLATIONS_OPEN: &str = "<sandbox_violations>";
const VIOLATIONS_CLOSE: &str = "</sandbox_violations>";
const NETWORK_DENIAL: &str = "deny network-outbound";
/// Wordings, besides [`OS_DENIALS`], that say the sandbox refused something.
const OTHER_DENIALS: [&str; 2] = ["CONNECT tunnel failed, response 403", "bwrap:"];
/// The file tools, whose results are not scanned: they are confined by
/// permission rules, not the OS sandbox, and what they print is file
/// content, which can say the denial words without any refusal. Every other
/// tool is scanned, so output that reaches the transcript under a name not
/// listed here (a background command's `BashOutput`, a tool a later harness
/// adds) keeps its evidence.
const FILE_TOOLS: [&str; 6] = ["Read", "Grep", "Glob", "Edit", "Write", "NotebookEdit"];
/// The harness's sentences for a refusal of a command's SHAPE, not of a
/// permission: the harness cannot analyse the command, or wants it split
/// (measured on Claude Code 2.1.288-2.1.289). They are kept here and not
/// beside [`PERMISSION_REFUSALS`] because the probe's pinned digest hashes
/// that module's marker sets. The full sentences, not fragments: a bare
/// "requires approval" could also be a real missing permission.
pub(crate) const SHAPE_REFUSALS: [&str; 6] = [
    "can't be checked before it runs",
    "This Bash command contains multiple operations. The following part requires approval",
    "This command requires approval",
    "Contains case_statement",
    "Contains brace with quote character",
    "Blocked: sleep",
];
const TEXT_LIMIT: usize = 200;
const REASON_LIMIT: usize = 400;
const LISTED: usize = 5;

/// One denial line and where it was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Denial {
    /// `tool_result:<tool_use_id>`, or the scratch file's path relative to
    /// the scratch (`check.log`, `claude-501/check.log`).
    pub source: String,
    /// The matching line, at most 200 characters.
    pub text: String,
}

/// How far the transcript can be trusted to hold every denial.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// A transcript was given, every line parsed and every `tool_use` has
    /// exactly one `tool_result`.
    Complete,
    Incomplete(String),
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenialReport {
    pub verified: Vec<Denial>,
    pub suspected: Vec<Denial>,
    pub coverage: Coverage,
}

/// What a refusal asks of the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalClass {
    /// The harness refused the command's form: a rewrite is accepted.
    Shape,
    /// A missing permission, or anything that cannot be shown to be a
    /// shape refusal.
    Capability,
}

/// One refused tool call, with the harness's reason from the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// `None` when the harness named no call for the denial.
    pub tool_use_id: Option<String>,
    /// The Bash command, empty when it is not known.
    pub command: String,
    /// The harness's refusal text, empty when it is not known.
    pub reason: String,
    pub class: RefusalClass,
}

/// A tool call the harness reported as denied.
#[derive(Debug, Clone, Copy)]
pub struct DeniedCall<'a> {
    pub tool: &'a str,
    pub tool_use_id: Option<&'a str>,
}

/// Where Claude Code keeps a session's transcript: under the project slug
/// of the directory it ran in, every character that is not ASCII
/// alphanumeric written `-` (measured: `/Users/x/.local/state/relais/
/// sandbox/probe/ab/worktree` is `-Users-x--local-state-relais-sandbox-
/// probe-ab-worktree`).
pub fn transcript_path(config_dir: &Path, work_dir: &Path, session_id: &str) -> PathBuf {
    let slug: String = work_dir
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    config_dir
        .join("projects")
        .join(slug)
        .join(format!("{session_id}.jsonl"))
}

fn short(line: &str) -> String {
    line.trim().chars().take(TEXT_LIMIT).collect()
}

fn is_network_denial(line: &str) -> bool {
    line.trim()
        .strip_prefix(NETWORK_DENIAL)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|target| target.rsplit_once(':'))
        .is_some_and(|(host, port)| !host.is_empty() && !port.is_empty())
}

fn reads_as_refusal(line: &str) -> bool {
    OS_DENIALS
        .iter()
        .chain(OTHER_DENIALS.iter())
        .any(|needle| line.contains(needle))
}

/// Sorts the lines of one text into `report`, each under `source`. Only a
/// network denial inside a `<sandbox_violations>` block is verified, and a
/// line is never both.
fn sort_lines(text: &str, source: &str, report: &mut DenialReport) {
    let mut in_block = false;
    for line in text.lines() {
        if line.contains(VIOLATIONS_OPEN) {
            in_block = true;
        }
        let denial = || Denial {
            source: source.to_string(),
            text: short(line),
        };
        if in_block && is_network_denial(line) {
            report.verified.push(denial());
        } else if reads_as_refusal(line) {
            report.suspected.push(denial());
        }
        if line.contains(VIOLATIONS_CLOSE) {
            in_block = false;
        }
    }
}

/// The text of a result's `content`; a result with none holds no denial
/// line, and content of any other shape is unreadable (`None`).
fn result_text(content: Option<&Value>) -> Option<String> {
    match content {
        None => Some(String::new()),
        Some(Value::String(text)) => Some(text.clone()),
        // An item with no text (an image) holds no denial line.
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Some(_) => None,
    }
}

/// The transcript's tool calls and results, and the first reason it cannot
/// be trusted to be whole.
#[derive(Default)]
struct Transcript {
    uses: Vec<String>,
    names: HashMap<String, String>,
    /// Each Bash call's command.
    commands: HashMap<String, String>,
    /// The text of each Bash result the harness marked `is_error`.
    errors: HashMap<String, String>,
    results: HashMap<String, usize>,
    problem: Option<String>,
}

impl Transcript {
    fn flag(&mut self, reason: String) {
        self.problem.get_or_insert(reason);
    }

    /// `Some(reason)` when a line was unreadable, a result was repeated or
    /// a call has no result.
    fn incompleteness(&self) -> Option<String> {
        if let Some(problem) = &self.problem {
            return Some(problem.clone());
        }
        if let Some((id, count)) = self.results.iter().find(|(_, count)| **count > 1) {
            return Some(format!("tool_use {id} has {count} tool_results"));
        }
        self.uses
            .iter()
            .find(|id| !self.results.contains_key(*id))
            .map(|id| format!("tool_use {id} has no tool_result"))
    }
}

fn read_item(item: &Value, transcript: &mut Transcript, report: &mut DenialReport, line_no: usize) {
    let field = |name: &str| item.get(name).and_then(Value::as_str);
    match field("type") {
        Some("tool_use") => match field("id") {
            Some(id) => {
                transcript.uses.push(id.to_string());
                if let Some(name) = field("name") {
                    transcript.names.insert(id.to_string(), name.to_string());
                }
                let command = item.pointer("/input/command").and_then(Value::as_str);
                if let (Some("Bash"), Some(command)) = (field("name"), command) {
                    transcript
                        .commands
                        .insert(id.to_string(), command.to_string());
                }
            }
            None => transcript.flag(format!("line {line_no}: tool_use without id")),
        },
        Some("tool_result") => {
            let Some(id) = field("tool_use_id") else {
                transcript.flag(format!("line {line_no}: tool_result without tool_use_id"));
                return;
            };
            *transcript.results.entry(id.to_string()).or_insert(0) += 1;
            // Only a known file tool is skipped: an unnamed or unlisted tool is scanned.
            let scanned = transcript
                .names
                .get(id)
                .is_none_or(|name| !FILE_TOOLS.contains(&name.as_str()));
            let bash = transcript.names.get(id).is_some_and(|name| name == "Bash");
            let is_error = item.get("is_error").and_then(Value::as_bool) == Some(true);
            match result_text(item.get("content")) {
                Some(text) if scanned => {
                    sort_lines(&text, &format!("tool_result:{id}"), report);
                    if bash && is_error {
                        transcript.errors.insert(id.to_string(), text);
                    }
                }
                Some(_) => {}
                None => transcript.flag(format!("line {line_no}: tool_result {id} has no text")),
            }
        }
        Some(_) | None => {}
    }
}

fn read_transcript(jsonl: &str, report: &mut DenialReport) -> Transcript {
    let mut transcript = Transcript::default();
    for (n, line) in jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_no = n + 1;
        match serde_json::from_str::<Value>(line) {
            Ok(value) if value.is_object() => {
                let items = value.pointer("/message/content").and_then(Value::as_array);
                for item in items.into_iter().flatten() {
                    read_item(item, &mut transcript, report, line_no);
                }
            }
            Ok(_) => transcript.flag(format!("line {line_no}: not a JSON object")),
            Err(err) => transcript.flag(format!("line {line_no}: malformed ({err})")),
        }
    }
    transcript
}

/// What the sandbox denied, from the transcript (when there is one) and
/// the scratch files as `(path relative to the scratch, text)`, the path
/// being each denial's source. Scratch files add suspected
/// denials and never change coverage: a worker's own logs are not the
/// harness's account.
pub fn scan(transcript_jsonl: Option<&str>, scratch_files: &[(PathBuf, String)]) -> DenialReport {
    let mut report = DenialReport {
        verified: Vec::new(),
        suspected: Vec::new(),
        coverage: Coverage::Unknown("no transcript".to_string()),
    };
    if let Some(jsonl) = transcript_jsonl {
        let transcript = read_transcript(jsonl, &mut report);
        report.coverage = match transcript.incompleteness() {
            Some(reason) => Coverage::Incomplete(reason),
            None => Coverage::Complete,
        };
    }
    for (path, text) in scratch_files {
        // `/`-joined on every platform, so a report reads the same anywhere.
        let source = path
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        sort_lines(text, &source, &mut report);
    }
    report
}

fn has_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// Shape only when the text positively says so and says nothing of a
/// missing permission; every other text is a capability refusal.
fn classify_text(text: &str) -> RefusalClass {
    if has_any(text, &PERMISSION_REFUSALS) || !has_any(text, &SHAPE_REFUSALS) {
        RefusalClass::Capability
    } else {
        RefusalClass::Shape
    }
}

/// Each denied call classified from the transcript, joined on the call's
/// id. Fail closed: a call is a shape refusal only when it is a Bash call
/// the transcript holds, whose `is_error` result carries a shape sentence
/// and no capability one, and the transcript is whole. A denial with no id
/// or an id the transcript lacks, a tool other than Bash, an unreadable or
/// partial transcript are all capability refusals.
pub fn classify_refusals(
    transcript_jsonl: Option<&str>,
    denied: &[DeniedCall<'_>],
) -> Vec<Refusal> {
    let transcript = transcript_jsonl.map(|jsonl| {
        let mut scratch = DenialReport {
            verified: Vec::new(),
            suspected: Vec::new(),
            coverage: Coverage::Complete,
        };
        let transcript = read_transcript(jsonl, &mut scratch);
        let whole = transcript.incompleteness().is_none();
        (transcript, whole)
    });
    denied
        .iter()
        .map(|call| {
            let known = transcript.as_ref().zip(call.tool_use_id);
            let command = known
                .and_then(|((read, _), id)| read.commands.get(id))
                .cloned()
                .unwrap_or_default();
            let reason = known
                .and_then(|((read, _), id)| read.errors.get(id))
                .cloned()
                .unwrap_or_default();
            let whole = transcript.as_ref().is_some_and(|(_, whole)| *whole);
            let class = if call.tool == "Bash" && whole && !command.is_empty() {
                classify_text(&reason)
            } else {
                RefusalClass::Capability
            };
            Refusal {
                tool_use_id: call.tool_use_id.map(str::to_string),
                command,
                reason: reason.trim().chars().take(REASON_LIMIT).collect(),
                class,
            }
        })
        .collect()
}

impl Coverage {
    fn describe(&self) -> String {
        match self {
            Self::Complete => "complete".to_string(),
            Self::Incomplete(reason) => format!("incomplete — {reason}"),
            Self::Unknown(reason) => format!("unknown — {reason}"),
        }
    }
}

impl DenialReport {
    /// The line said for a recorded report that cannot be read, within 80
    /// columns.
    pub fn render_unreadable(why: &str) -> String {
        clip(&format!("sandbox denials: unreadable ({why})"))
    }

    /// The summary line, then up to five lines of each kind, each within
    /// 80 columns.
    pub fn render(&self) -> String {
        let mut lines = vec![clip(&format!(
            "sandbox denials: {} verified, {} suspected (coverage: {})",
            self.verified.len(),
            self.suspected.len(),
            self.coverage.describe()
        ))];
        for (kind, denials) in [("verified", &self.verified), ("suspected", &self.suspected)] {
            for denial in denials.iter().take(LISTED) {
                lines.push(clip(&format!(
                    "  {kind} {}: {}",
                    denial.source, denial.text
                )));
            }
            if denials.len() > LISTED {
                lines.push(format!("  … {} more {kind}", denials.len() - LISTED));
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(value: Value) -> String {
        value.to_string()
    }

    fn tool_use(id: &str) -> String {
        named_tool_use(id, "Bash")
    }

    fn named_tool_use(id: &str, name: &str) -> String {
        line(serde_json::json!({"message": {"content": [
            {"type": "tool_use", "id": id, "name": name, "input": {}}
        ]}}))
    }

    fn tool_result(id: &str, text: &str) -> String {
        line(serde_json::json!({"message": {"content": [
            {"type": "tool_result", "tool_use_id": id, "content": text}
        ]}}))
    }

    fn transcript(lines: &[String]) -> String {
        lines.join("\n")
    }

    fn bash_call(id: &str, command: &str) -> String {
        line(serde_json::json!({"message": {"content": [
            {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}
        ]}}))
    }

    fn error_result(id: &str, text: &str) -> String {
        line(serde_json::json!({"message": {"content": [
            {"type": "tool_result", "tool_use_id": id, "content": text, "is_error": true}
        ]}}))
    }

    fn classify_one(lines: &[String], tool: &str, id: Option<&str>) -> Refusal {
        let jsonl = transcript(lines);
        let denied = [DeniedCall {
            tool,
            tool_use_id: id,
        }];
        let mut refusals = classify_refusals(Some(&jsonl), &denied);
        assert_eq!(refusals.len(), 1);
        refusals.remove(0)
    }

    fn class_of_text(text: &str) -> RefusalClass {
        let lines = [bash_call("t1", "x"), error_result("t1", text)];
        classify_one(&lines, "Bash", Some("t1")).class
    }

    const VIOLATION: &str = "Exit code 56\n<sandbox_violations>\ndeny network-outbound \
                             evil.test:443 (host is not on the allow list)\n</sandbox_violations>";

    #[test]
    fn a_network_violation_block_is_one_verified_denial() {
        let jsonl = transcript(&[tool_use("t1"), tool_result("t1", VIOLATION)]);
        let report = scan(Some(&jsonl), &[]);
        assert_eq!(report.verified.len(), 1, "{report:?}");
        assert_eq!(report.verified[0].source, "tool_result:t1");
        assert!(report.verified[0].text.starts_with("deny network-outbound"));
        assert!(report.suspected.is_empty(), "{report:?}");
        assert_eq!(report.coverage, Coverage::Complete);
    }

    #[test]
    fn a_network_line_outside_the_block_is_not_verified() {
        let text = "deny network-outbound evil.test:443\nnothing else";
        let jsonl = transcript(&[tool_use("t1"), tool_result("t1", text)]);
        let report = scan(Some(&jsonl), &[]);
        assert!(report.verified.is_empty(), "{report:?}");
    }

    #[test]
    fn a_filesystem_refusal_is_suspected() {
        let text = "touch: /tmp/x: Operation not permitted";
        let jsonl = transcript(&[tool_use("t1"), tool_result("t1", text)]);
        let report = scan(Some(&jsonl), &[]);
        assert!(report.verified.is_empty());
        assert_eq!(report.suspected.len(), 1);
        assert_eq!(report.suspected[0].source, "tool_result:t1");
    }

    #[test]
    fn the_same_text_in_a_scratch_log_is_suspected_with_the_file_as_source() {
        // Paths come relative to the scratch; the source is that path.
        let files = vec![(
            PathBuf::from("claude-501/build.log"),
            "cp: Read-only file system\nfine".to_string(),
        )];
        let report = scan(None, &files);
        assert!(report.verified.is_empty());
        assert_eq!(report.suspected.len(), 1);
        assert_eq!(report.suspected[0].source, "claude-501/build.log");
        assert_eq!(report.suspected[0].text, "cp: Read-only file system");
    }

    #[test]
    fn an_application_error_with_the_words_stays_suspected() {
        let text = "Exit code 1\nmy-app: Operation not permitted by policy";
        let jsonl = transcript(&[tool_use("t1"), tool_result("t1", text)]);
        let report = scan(Some(&jsonl), &[]);
        assert!(report.verified.is_empty(), "{report:?}");
        assert_eq!(report.suspected.len(), 1);
    }

    #[test]
    fn a_proxy_refusal_and_a_bwrap_failure_are_suspected() {
        let text = "curl: CONNECT tunnel failed, response 403\nbwrap: Can't mount proc";
        let jsonl = transcript(&[tool_use("t1"), tool_result("t1", text)]);
        assert_eq!(scan(Some(&jsonl), &[]).suspected.len(), 2);
    }

    #[test]
    fn a_line_is_never_both_verified_and_suspected() {
        let text =
            "<sandbox_violations>\ndeny network-outbound a.test:443 operation not permitted\n\
                    </sandbox_violations>";
        let jsonl = transcript(&[tool_use("t1"), tool_result("t1", text)]);
        let report = scan(Some(&jsonl), &[]);
        assert_eq!(report.verified.len(), 1);
        assert!(report.suspected.is_empty(), "{report:?}");
    }

    #[test]
    fn file_tool_results_with_denial_words_add_nothing() {
        let text = "fixture: Operation not permitted\nRead-only file system";
        for name in FILE_TOOLS {
            let jsonl = transcript(&[named_tool_use("t1", name), tool_result("t1", text)]);
            let report = scan(Some(&jsonl), &[]);
            assert!(report.suspected.is_empty(), "{name}: {report:?}");
            assert!(report.verified.is_empty(), "{name}: {report:?}");
            assert_eq!(report.coverage, Coverage::Complete, "{name}");
        }
    }

    #[test]
    fn a_tool_outside_the_file_tools_is_scanned() {
        let text = "fixture: Operation not permitted";
        for name in ["BashOutput", "PowerShell", "Monitor", "SomeFutureTool"] {
            let jsonl = transcript(&[named_tool_use("t1", name), tool_result("t1", text)]);
            assert_eq!(scan(Some(&jsonl), &[]).suspected.len(), 1, "{name}");
        }
    }

    #[test]
    fn a_bash_result_with_the_same_text_is_suspected() {
        let text = "fixture: Operation not permitted";
        let jsonl = transcript(&[named_tool_use("t1", "Bash"), tool_result("t1", text)]);
        assert_eq!(scan(Some(&jsonl), &[]).suspected.len(), 1);
    }

    #[test]
    fn a_violation_block_in_a_bash_result_is_still_verified() {
        let jsonl = transcript(&[named_tool_use("t1", "Bash"), tool_result("t1", VIOLATION)]);
        assert_eq!(scan(Some(&jsonl), &[]).verified.len(), 1);
    }

    #[test]
    fn a_result_whose_tool_use_is_missing_is_scanned_and_incomplete_as_before() {
        let jsonl = transcript(&[
            tool_use("t2"),
            tool_result("t1", "x: Operation not permitted"),
        ]);
        let report = scan(Some(&jsonl), &[]);
        assert_eq!(report.suspected.len(), 1, "{report:?}");
        assert!(matches!(report.coverage, Coverage::Incomplete(_)));
    }

    #[test]
    fn a_tool_use_with_no_name_is_scanned() {
        let unnamed = line(serde_json::json!({"message": {"content": [
            {"type": "tool_use", "id": "t1", "input": {}}
        ]}}));
        let jsonl = transcript(&[unnamed, tool_result("t1", "x: Operation not permitted")]);
        assert_eq!(scan(Some(&jsonl), &[]).suspected.len(), 1);
    }

    #[test]
    fn a_tool_use_without_a_result_is_incomplete() {
        let jsonl = transcript(&[tool_use("t1"), tool_use("t2"), tool_result("t1", "ok")]);
        let Coverage::Incomplete(reason) = scan(Some(&jsonl), &[]).coverage else {
            panic!("expected incomplete");
        };
        assert!(
            reason.contains("t2") && reason.contains("no tool_result"),
            "{reason}"
        );
    }

    #[test]
    fn a_duplicate_tool_result_is_incomplete() {
        let jsonl = transcript(&[
            tool_use("t1"),
            tool_result("t1", "ok"),
            tool_result("t1", "again"),
        ]);
        let Coverage::Incomplete(reason) = scan(Some(&jsonl), &[]).coverage else {
            panic!("expected incomplete");
        };
        assert!(
            reason.contains("t1") && reason.contains("2 tool_results"),
            "{reason}"
        );
    }

    #[test]
    fn a_tool_result_with_no_content_is_empty_text_not_incomplete() {
        let bare = line(serde_json::json!({"message": {"content": [
            {"type": "tool_result", "tool_use_id": "t1"}
        ]}}));
        let jsonl = transcript(&[tool_use("t1"), bare]);
        let report = scan(Some(&jsonl), &[]);
        assert_eq!(report.coverage, Coverage::Complete, "{report:?}");
        assert!(report.verified.is_empty() && report.suspected.is_empty());
    }

    #[test]
    fn a_tool_result_with_content_of_another_shape_is_incomplete() {
        let odd = line(serde_json::json!({"message": {"content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": 7}
        ]}}));
        let jsonl = transcript(&[tool_use("t1"), odd]);
        assert!(matches!(
            scan(Some(&jsonl), &[]).coverage,
            Coverage::Incomplete(_)
        ));
    }

    #[test]
    fn an_unreadable_report_line_is_within_80_columns() {
        let rendered = DenialReport::render_unreadable(&"e".repeat(300));
        assert_eq!(rendered.chars().count(), 80, "{rendered}");
        assert!(rendered.starts_with("sandbox denials: unreadable ("));
    }

    #[test]
    fn a_malformed_line_is_incomplete_and_the_rest_is_still_scanned() {
        let jsonl = transcript(&[
            tool_use("t1"),
            "{\"message\": {\"cont".to_string(),
            tool_result("t1", VIOLATION),
        ]);
        let report = scan(Some(&jsonl), &[]);
        assert!(
            matches!(&report.coverage, Coverage::Incomplete(r) if r.contains("line 2")),
            "{report:?}"
        );
        assert_eq!(report.verified.len(), 1);
    }

    #[test]
    fn no_transcript_is_unknown_and_scratch_files_never_change_coverage() {
        let files = vec![(PathBuf::from("a.log"), "fine".to_string())];
        let report = scan(None, &files);
        assert_eq!(
            report.coverage,
            Coverage::Unknown("no transcript".to_string())
        );
        let jsonl = transcript(&[tool_use("t1"), tool_result("t1", "ok")]);
        assert_eq!(scan(Some(&jsonl), &files).coverage, Coverage::Complete);
    }

    #[test]
    fn a_long_line_is_cut_to_200_characters() {
        let text = format!("Operation not permitted {}", "x".repeat(400));
        let jsonl = transcript(&[tool_use("t1"), tool_result("t1", &text)]);
        assert_eq!(
            scan(Some(&jsonl), &[]).suspected[0].text.chars().count(),
            200
        );
    }

    #[test]
    fn the_transcript_path_replaces_every_non_alphanumeric_with_a_dash() {
        let path = transcript_path(
            Path::new("/cfg"),
            Path::new("/Users/x/.local/state/relais/sandbox/probe/ab/worktree"),
            "sess",
        );
        assert_eq!(
            path,
            Path::new(
                "/cfg/projects/-Users-x--local-state-relais-sandbox-probe-ab-worktree/sess.jsonl"
            )
        );
    }

    #[test]
    fn render_says_the_counts_and_the_coverage_within_80_columns() {
        let denial = |n: usize| Denial {
            source: format!("tool_result:toolu_{n}"),
            text: "x".repeat(300),
        };
        let report = DenialReport {
            verified: (0..7).map(denial).collect(),
            suspected: vec![denial(9)],
            coverage: Coverage::Incomplete("tool_use toolu_long has no tool_result".to_string()),
        };
        let rendered = report.render();
        assert!(
            rendered.starts_with("sandbox denials: 7 verified, 1 suspected (coverage: incomplete"),
            "{rendered}"
        );
        assert_eq!(rendered.lines().count(), 1 + 5 + 1 + 1, "{rendered}");
        assert!(rendered.contains("… 2 more verified"), "{rendered}");
        for line in rendered.lines() {
            assert!(line.chars().count() <= 80, "{line}");
        }
    }

    #[test]
    fn render_names_each_coverage() {
        let render = |coverage| {
            DenialReport {
                verified: vec![],
                suspected: vec![],
                coverage,
            }
            .render()
        };
        assert_eq!(
            render(Coverage::Complete),
            "sandbox denials: 0 verified, 0 suspected (coverage: complete)"
        );
        assert_eq!(
            render(Coverage::Unknown("no transcript".into())),
            "sandbox denials: 0 verified, 0 suspected (coverage: unknown — no transcript)"
        );
    }

    #[test]
    fn each_measured_shape_text_is_a_shape_refusal() {
        for text in [
            "A variable/file redirect in this command can't be checked before it runs",
            "This Bash command contains multiple operations. The following part requires approval: tail x",
            "This command requires approval",
            "Contains case_statement",
            "Contains brace with quote character",
            "Blocked: sleep 5 is not allowed",
        ] {
            assert_eq!(class_of_text(text), RefusalClass::Shape, "{text}");
        }
    }

    #[test]
    fn each_measured_capability_text_is_a_capability_refusal() {
        for text in [
            "Permission to use Bash has been denied.",
            "path is outside; --restricted confines the file tools to the working directory.",
            "Bash(git push) was denied by your permission settings",
        ] {
            assert_eq!(class_of_text(text), RefusalClass::Capability, "{text}");
        }
    }

    #[test]
    fn a_shape_refusal_keeps_its_command_and_reason() {
        let lines = [
            bash_call("t1", "cat <<E > $TMPDIR/x"),
            error_result("t1", "This command requires approval"),
        ];
        let refusal = classify_one(&lines, "Bash", Some("t1"));
        assert_eq!(refusal.command, "cat <<E > $TMPDIR/x");
        assert_eq!(refusal.reason, "This command requires approval");
        assert_eq!(refusal.tool_use_id.as_deref(), Some("t1"));
    }

    #[test]
    fn an_unknown_text_and_a_text_in_both_sets_are_capability() {
        assert_eq!(class_of_text("something else"), RefusalClass::Capability);
        let both = "This command requires approval; Bash has been denied";
        assert_eq!(class_of_text(both), RefusalClass::Capability);
    }

    #[test]
    fn a_denial_without_a_known_call_is_capability() {
        let lines = [
            bash_call("t1", "x"),
            error_result("t1", "Contains case_statement"),
        ];
        assert_eq!(
            classify_one(&lines, "Bash", None).class,
            RefusalClass::Capability
        );
        assert_eq!(
            classify_one(&lines, "Bash", Some("nope")).class,
            RefusalClass::Capability
        );
    }

    #[test]
    fn a_non_bash_denial_is_capability() {
        let lines = [
            bash_call("t1", "x"),
            error_result("t1", "Contains case_statement"),
        ];
        assert_eq!(
            classify_one(&lines, "Edit", Some("t1")).class,
            RefusalClass::Capability
        );
    }

    #[test]
    fn a_result_that_is_not_an_error_is_capability() {
        let lines = [
            bash_call("t1", "x"),
            tool_result("t1", "Contains case_statement"),
        ];
        assert_eq!(
            classify_one(&lines, "Bash", Some("t1")).class,
            RefusalClass::Capability
        );
    }

    #[test]
    fn incomplete_or_missing_coverage_makes_every_refusal_capability() {
        let lines = [
            bash_call("t1", "x"),
            error_result("t1", "Contains case_statement"),
            bash_call("t2", "y"),
        ];
        assert_eq!(
            classify_one(&lines, "Bash", Some("t1")).class,
            RefusalClass::Capability
        );
        let denied = [DeniedCall {
            tool: "Bash",
            tool_use_id: Some("t1"),
        }];
        let refusals = classify_refusals(None, &denied);
        assert_eq!(refusals[0].class, RefusalClass::Capability);
    }

    #[test]
    fn a_long_reason_is_cut_after_it_is_classified() {
        let text = format!("{} Bash has been denied", "x ".repeat(300));
        assert_eq!(class_of_text(&text), RefusalClass::Capability);
        let lines = [bash_call("t1", "x"), error_result("t1", &text)];
        let refusal = classify_one(&lines, "Bash", Some("t1"));
        assert_eq!(refusal.reason.chars().count(), REASON_LIMIT);
    }
}
