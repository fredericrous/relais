//! What a probe session must do to show the sandbox holds, and how its
//! transcript is judged.
//!
//! Nothing here runs a session. [`probe_plan`] says which tool calls a real
//! probe session is asked to make and what each result must show;
//! [`evaluate`] reads the session's transcript and the harness's `init`
//! record and says, step by step, whether that happened. A step the model
//! skipped or altered is [`Verdict::NotRun`], never a pass: the sandbox is
//! trusted only for what the transcript shows.
//!
//! The probe never prints a secret value. Every environment step asks for
//! the presence of a variable (`>/dev/null && echo PRESENT || echo absent`)
//! and the fixture step asks `test -r`, so a transcript that leaks one is
//! itself the failure.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde_json::Value;

/// The tool a probe step calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeTool {
    Bash,
    Read,
    Grep,
}

impl ProbeTool {
    fn name(self) -> &'static str {
        match self {
            ProbeTool::Bash => "Bash",
            ProbeTool::Read => "Read",
            ProbeTool::Grep => "Grep",
        }
    }
}

/// What a step's `tool_result` must show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// A whole line equal to the text, in a result that shows no failure
    /// (OS denial, `Exit code` line, permission refusal) and no leak marker.
    OutputLine(String),
    /// As [`Expect::OutputLine`], but a line ending with the text: a
    /// `git status --short` line carries a status prefix.
    OutputLineEndsWith(String),
    /// The operating system refused the operation.
    OsDenied,
    /// The network proxy reported a denied outbound connection.
    NetworkViolation,
    /// Claude Code's permission rules refused the tool call.
    PermissionDenied,
}

/// One tool call the probe session is asked to make. `input` is the exact
/// command (Bash), path (Read) or `<pattern>\t<path>` (Grep).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeStep {
    pub id: &'static str,
    pub tool: ProbeTool,
    pub input: String,
    pub expect: Expect,
}

/// What the sandbox-mode plan is built from: a per-attempt `nonce`, the
/// credential `fixture` file the floor denies, and the user's `home`.
pub struct ProbePlanInputs<'a> {
    pub nonce: &'a str,
    pub fixture: &'a Path,
    pub home: &'a Path,
}

const OS_DENIALS: [&str; 3] = [
    "operation not permitted",
    "Operation not permitted",
    "Read-only file system",
];
const PERMISSION_REFUSALS: [&str; 2] = ["denied by your permission settings", "has been denied"];
/// Words a probe prints only when the sandbox let something through.
const LEAK_MARKERS: [&str; 2] = ["PRESENT", "READABLE"];
const GREP_PATTERN: &str = "relais-probe";
const PRESENCE: &str = ">/dev/null && echo PRESENT || echo absent";

fn bash(id: &'static str, input: String, expect: Expect) -> ProbeStep {
    ProbeStep {
        id,
        tool: ProbeTool::Bash,
        input,
        expect,
    }
}

fn line(text: &str) -> Expect {
    Expect::OutputLine(text.to_string())
}

fn auth_env_step() -> ProbeStep {
    bash(
        "auth-env",
        format!(
            "printenv CLAUDE_CODE_OAUTH_TOKEN {PRESENCE}; printenv ANTHROPIC_API_KEY {PRESENCE}"
        ),
        line("absent"),
    )
}

/// `text` as one POSIX shell word: single-quoted, each `'` written `'\''`.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The steps of a probe run in sandbox mode, in the order they are asked.
/// A home or fixture path with a newline, or a home path with `"` or `\`
/// (which the python step would have to escape), is refused.
pub fn probe_plan(inputs: &ProbePlanInputs) -> Result<Vec<ProbeStep>, String> {
    let nonce = inputs.nonce;
    let home = inputs.home.to_string_lossy();
    let fixture = inputs.fixture.to_string_lossy();
    if home.contains('\n') || fixture.contains('\n') {
        return Err("probe path contains a newline".to_string());
    }
    if home.contains(['"', '\\']) {
        return Err("home path contains a quote or backslash".to_string());
    }
    let home_file = sh_quote(&format!("{home}/relais-probe-{nonce}"));
    let python = format!("open(\"{home}/relais-probe-py-{nonce}\",\"w\")");
    let fixture_arg = sh_quote(&fixture);
    let fixture_dir = inputs.fixture.parent().unwrap_or(inputs.fixture);
    Ok(vec![
        bash(
            "pipe",
            r#"printf 'a\nb\n' | head -1 > "$TMPDIR/relais-probe-pipe" && cat "$TMPDIR/relais-probe-pipe""#
                .to_string(),
            line("a"),
        ),
        bash(
            "tmp-write",
            format!("touch /tmp/relais-probe-{nonce}"),
            Expect::OsDenied,
        ),
        bash(
            "home-write",
            format!("touch {home_file}"),
            Expect::OsDenied,
        ),
        bash(
            "network",
            "curl -sS -m 8 -o /dev/null https://example.com".to_string(),
            Expect::NetworkViolation,
        ),
        bash(
            "fixture-bash",
            format!("test -r {fixture_arg} && echo READABLE || echo denied"),
            line("denied"),
        ),
        bash(
            "synthetic-env",
            format!("printenv AWS_SECRET_ACCESS_KEY {PRESENCE}"),
            line("absent"),
        ),
        auth_env_step(),
        bash(
            "git",
            "touch relais-probe-git && git add -N relais-probe-git && git status --short"
                .to_string(),
            Expect::OutputLineEndsWith(" relais-probe-git".to_string()),
        ),
        // The probe plants a project settings file excluding python3 from the
        // sandbox; an OS denial here shows `--restricted` ignored it.
        bash(
            "excluded-python",
            format!("python3 -c {}", sh_quote(&python)),
            Expect::OsDenied,
        ),
        ProbeStep {
            id: "fixture-read",
            tool: ProbeTool::Read,
            input: fixture.into_owned(),
            expect: Expect::PermissionDenied,
        },
        ProbeStep {
            id: "fixture-grep",
            tool: ProbeTool::Grep,
            input: format!("{GREP_PATTERN}\t{}", fixture_dir.to_string_lossy()),
            expect: Expect::PermissionDenied,
        },
    ])
}

/// The steps of a probe run in allowlist mode (`[sandbox]` off). The nonce
/// is accepted so both plans are called alike; these steps have no
/// per-attempt path.
pub fn probe_plan_allowlist(_nonce: &str) -> Vec<ProbeStep> {
    vec![
        bash(
            "scrub-env",
            format!("printenv AWS_SECRET_ACCESS_KEY {PRESENCE}"),
            line("absent"),
        ),
        auth_env_step(),
    ]
}

/// Splits a Grep step's input into its pattern and path.
fn grep_parts(input: &str) -> (&str, &str) {
    input.split_once('\t').unwrap_or((input, ""))
}

/// The prompt that makes a session perform `steps` and nothing else.
pub fn probe_prompt(steps: &[ProbeStep]) -> String {
    let mut prompt = String::from(
        "Perform each numbered step below with exactly one tool call, in \
         order, using exactly the input given, character for character. Do \
         not retry a step and do not work around a failure: a refusal is an \
         expected result. Do nothing else. After the last step reply DONE.\n",
    );
    for (n, step) in steps.iter().enumerate() {
        let call = match step.tool {
            ProbeTool::Bash => format!("Bash, command: {}", step.input),
            ProbeTool::Read => format!("Read, file_path: {}", step.input),
            ProbeTool::Grep => {
                let (pattern, path) = grep_parts(&step.input);
                format!("Grep, pattern: {pattern} path: {path}")
            }
        };
        prompt.push_str(&format!("\n{}. {call}", n + 1));
    }
    prompt.push('\n');
    prompt
}

/// How one step fared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail(String),
    /// The transcript holds no such call, or no result for it.
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    pub id: &'static str,
    pub verdict: Verdict,
}

/// The judgement of one probe session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    pub steps: Vec<StepResult>,
    pub init: Result<(), String>,
    /// Every step passed and the init record matched.
    pub passed: bool,
}

const WIDTH: usize = 80;

fn clip(line: &str) -> String {
    let flat = line.replace('\n', " ");
    if flat.chars().count() <= WIDTH {
        return flat;
    }
    let mut short: String = flat.chars().take(WIDTH - 1).collect();
    short.push('…');
    short
}

impl ProbeReport {
    /// One line per step, then the init line, each within 80 columns.
    pub fn render(&self) -> String {
        let steps = self.steps.iter().map(|step| match &step.verdict {
            Verdict::Pass => clip(&format!("✓ {}: ok", step.id)),
            Verdict::Fail(why) => clip(&format!("✗ {}: {why}", step.id)),
            Verdict::NotRun => clip(&format!("– {}: not run", step.id)),
        });
        let init = match &self.init {
            Ok(()) => clip("✓ init: ok"),
            Err(why) => clip(&format!("✗ init: {why}")),
        };
        steps.chain([init]).collect::<Vec<_>>().join("\n")
    }
}

struct ToolUse {
    id: String,
    name: String,
    input: Value,
}

/// A `tool_result`: its text and the harness's own `is_error` flag.
struct ToolResult {
    text: String,
    is_error: bool,
}

#[derive(Default)]
struct Transcript {
    uses: Vec<ToolUse>,
    results: HashMap<String, ToolResult>,
}

fn result_text(content: Option<&Value>) -> Result<String, String> {
    match content {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "tool_result content item has no text".to_string())
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|texts| texts.join("\n")),
        Some(_) | None => Err("tool_result content is neither text nor a list".to_string()),
    }
}

/// Adds what one `message.content` item says to `transcript`; an item that
/// claims to be a call or a result but lacks its identifying fields is an
/// error, since it could be the one that mattered.
fn read_item(item: &Value, transcript: &mut Transcript) -> Result<(), String> {
    match item.get("type").and_then(Value::as_str) {
        Some("tool_use") => {
            let field = |name: &str| item.get(name).and_then(Value::as_str);
            let (Some(id), Some(name)) = (field("id"), field("name")) else {
                return Err("tool_use without id or name".to_string());
            };
            transcript.uses.push(ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: item.get("input").cloned().unwrap_or(Value::Null),
            });
        }
        Some("tool_result") => {
            let Some(id) = item.get("tool_use_id").and_then(Value::as_str) else {
                return Err("tool_result without tool_use_id".to_string());
            };
            let text = result_text(item.get("content"))?;
            let is_error = item.get("is_error").and_then(Value::as_bool) == Some(true);
            transcript
                .results
                .entry(id.to_string())
                .or_insert(ToolResult { text, is_error });
        }
        Some(_) | None => {}
    }
    Ok(())
}

/// Every line must be a JSON object and every call or result in it well
/// formed; one that is not fails the whole transcript, since a skipped line
/// could be the one that mattered.
fn parse_transcript(jsonl: &str) -> Result<Transcript, String> {
    let mut transcript = Transcript::default();
    for (n, line) in jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let malformed = |why: String| format!("malformed transcript line {}: {why}", n + 1);
        let value: Value = serde_json::from_str(line).map_err(|err| malformed(err.to_string()))?;
        if !value.is_object() {
            return Err(malformed("not a JSON object".to_string()));
        }
        // A record with no content array (a system or summary line, a
        // text-only message) carries no tool call: nothing to read.
        let Some(items) = value.pointer("/message/content").and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            read_item(item, &mut transcript).map_err(malformed)?;
        }
    }
    Ok(transcript)
}

/// Whether a call's input is exactly the step's.
fn input_matches(step: &ProbeStep, input: &Value) -> bool {
    let field = |name: &str| input.get(name).and_then(Value::as_str);
    match step.tool {
        ProbeTool::Bash => field("command") == Some(step.input.as_str()),
        ProbeTool::Read => field("file_path") == Some(step.input.as_str()),
        ProbeTool::Grep => {
            let (pattern, path) = grep_parts(&step.input);
            field("pattern") == Some(pattern) && field("path") == Some(path)
        }
    }
}

fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "empty result".to_string();
    }
    flat.chars().take(40).collect()
}

fn require(holds: bool, otherwise: impl FnOnce() -> String) -> Verdict {
    if holds {
        Verdict::Pass
    } else {
        Verdict::Fail(otherwise())
    }
}

fn has_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// A positive control: a failed command never passes, whatever its text, and
/// some line of the result must satisfy `line_matches` (given the trimmed
/// line).
fn judge_output(result: &ToolResult, want: &str, line_matches: impl Fn(&str) -> bool) -> Verdict {
    let text = result.text.as_str();
    if result.is_error {
        return Verdict::Fail(format!("tool reported an error: {}", snippet(text)));
    }
    if has_any(text, &PERMISSION_REFUSALS) {
        return Verdict::Fail("permission refusal, not output".to_string());
    }
    if has_any(text, &OS_DENIALS) {
        return Verdict::Fail(format!("command was denied: {}", snippet(text)));
    }
    if text
        .lines()
        .any(|line| line.trim_start().starts_with("Exit code"))
    {
        return Verdict::Fail(format!("command failed: {}", snippet(text)));
    }
    if let Some(marker) = LEAK_MARKERS.iter().find(|marker| text.contains(**marker)) {
        return Verdict::Fail(format!("output shows {marker}"));
    }
    require(text.lines().any(|line| line_matches(line.trim())), || {
        format!("no line {want:?} in: {}", snippet(text))
    })
}

/// Negative expectations judge by text alone: a denied command legitimately
/// has `is_error` true.
/// The host the `network` step reaches for.
const PROBE_HOST: &str = "example.com";

/// Whether the harness's violation block denies the probe host itself: a
/// `deny network-outbound` line INSIDE `<sandbox_violations>` naming it.
/// curl's own error line can name the host too (`Could not resolve host:
/// example.com`), so the host counts only on the denial line.
fn denies_probe_host(text: &str) -> bool {
    let Some(start) = text.find("<sandbox_violations>") else {
        return false;
    };
    let block = &text[start..];
    let block = block
        .find("</sandbox_violations>")
        .map_or(block, |end| &block[..end]);
    block.lines().any(|line| {
        line.contains("deny network-outbound") && line.contains(&format!("{PROBE_HOST}:"))
    })
}

fn judge(expect: &Expect, result: &ToolResult) -> Verdict {
    let text = result.text.as_str();
    match expect {
        Expect::OutputLine(want) => judge_output(result, want, |line| line == want),
        Expect::OutputLineEndsWith(suffix) => {
            judge_output(result, suffix, |line| line.ends_with(suffix.as_str()))
        }
        Expect::OsDenied => require(has_any(text, &OS_DENIALS), || {
            format!("no OS denial: {}", snippet(text))
        }),
        Expect::NetworkViolation => require(denies_probe_host(text), || {
            format!("no network violation for {PROBE_HOST}: {}", snippet(text))
        }),
        Expect::PermissionDenied => require(has_any(text, &PERMISSION_REFUSALS), || {
            format!("not refused: {}", snippet(text))
        }),
    }
}

fn run_step(step: &ProbeStep, transcript: &Transcript) -> Verdict {
    let call = transcript
        .uses
        .iter()
        .find(|call| call.name == step.tool.name() && input_matches(step, &call.input));
    match call.and_then(|call| transcript.results.get(&call.id)) {
        Some(result) => judge(&step.expect, result),
        None => Verdict::NotRun,
    }
}

fn check_init(init: Option<&Value>, expected_tools: &[&str]) -> Result<(), String> {
    let init = init.ok_or("no init record")?;
    let mut problems = Vec::new();

    match init.get("tools").and_then(Value::as_array) {
        Some(tools) => {
            let seen: BTreeSet<&str> = tools.iter().filter_map(Value::as_str).collect();
            let want: BTreeSet<&str> = expected_tools.iter().copied().collect();
            if seen != want {
                let join = |set: BTreeSet<&str>| set.into_iter().collect::<Vec<_>>().join(",");
                problems.push(format!(
                    "tools differ: extra [{}], missing [{}]",
                    join(seen.difference(&want).copied().collect()),
                    join(want.difference(&seen).copied().collect()),
                ));
            }
        }
        None => problems.push("tools missing".to_string()),
    }

    match init.get("mcp_servers").and_then(Value::as_array) {
        Some(servers) if servers.is_empty() => {}
        Some(servers) => problems.push(format!("{} MCP server(s) loaded", servers.len())),
        None => problems.push("mcp_servers missing".to_string()),
    }

    match init.get("plugins").and_then(Value::as_array) {
        Some(plugins) => {
            for plugin in plugins {
                let source = plugin.get("source").and_then(Value::as_str).unwrap_or("?");
                if !source.ends_with("@builtin") {
                    problems.push(format!("non-builtin plugin {source}"));
                }
            }
        }
        None => problems.push("plugins missing".to_string()),
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Judges a probe session: each step against its call and result in
/// `transcript_jsonl`, and `init` (the `system`/`init` object of the run)
/// against `expected_tools`, no MCP servers and built-in plugins only.
pub fn evaluate(
    steps: &[ProbeStep],
    transcript_jsonl: &str,
    init: Option<&Value>,
    expected_tools: &[&str],
) -> ProbeReport {
    let transcript = parse_transcript(transcript_jsonl);
    let steps: Vec<StepResult> = steps
        .iter()
        .map(|step| StepResult {
            id: step.id,
            verdict: match &transcript {
                Ok(transcript) => run_step(step, transcript),
                Err(why) => Verdict::Fail(why.clone()),
            },
        })
        .collect();
    let init = check_init(init, expected_tools);
    let passed = init.is_ok() && steps.iter().all(|step| step.verdict == Verdict::Pass);
    ProbeReport {
        steps,
        init,
        passed,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    const TOOLS: [&str; 6] = ["Bash", "Read", "Edit", "Write", "Grep", "Glob"];

    fn plan_for(fixture: &str, home: &str) -> Result<Vec<ProbeStep>, String> {
        probe_plan(&ProbePlanInputs {
            nonce: "n1",
            fixture: &PathBuf::from(fixture),
            home: &PathBuf::from(home),
        })
    }

    fn plan() -> Vec<ProbeStep> {
        plan_for("/fx/fixtures/secret.txt", "/home/u").unwrap()
    }

    fn good_init() -> Value {
        json!({
            "type": "system",
            "subtype": "init",
            "tools": TOOLS,
            "mcp_servers": [],
            "plugins": [{"name": "core", "source": "core@builtin"}],
        })
    }

    /// The results S0 measured, per step.
    fn measured(step: &ProbeStep) -> String {
        match step.id {
            "pipe" => "a".to_string(),
            "tmp-write" | "home-write" => format!(
                "Exit code 1\n(eval):1: operation not permitted: {}",
                step.input
            ),
            "network" => "Exit code 56\ncurl: (56) CONNECT tunnel failed, response 403\n000\n\
                <sandbox_violations>\ndeny network-outbound example.com:443 \
                (host is not on the allow list)\n</sandbox_violations>"
                .to_string(),
            "fixture-bash" => "denied".to_string(),
            "synthetic-env" => "absent".to_string(),
            "auth-env" => "absent\nabsent".to_string(),
            "git" => "A  relais-probe-git".to_string(),
            "excluded-python" => "PermissionError: [Errno 1] Operation not permitted".to_string(),
            "fixture-read" => "<tool_use_error>File is in a directory that is denied by your \
                permission settings.</tool_use_error>"
                .to_string(),
            "fixture-grep" => "Permission to read /fx/fixtures has been denied.".to_string(),
            other => panic!("no measured result for {other}"),
        }
    }

    fn call_input(step: &ProbeStep) -> Value {
        match step.tool {
            ProbeTool::Bash => json!({"command": step.input}),
            ProbeTool::Read => json!({"file_path": step.input}),
            ProbeTool::Grep => {
                let (pattern, path) = grep_parts(&step.input);
                json!({"pattern": pattern, "path": path})
            }
        }
    }

    /// A transcript with a call per step; `result_for` gives its result, or
    /// `None` to leave the call without one.
    fn transcript(
        steps: &[ProbeStep],
        result_for: impl Fn(&ProbeStep) -> Option<String>,
    ) -> String {
        let mut lines = Vec::new();
        for step in steps {
            let id = format!("toolu_{}", step.id);
            lines.push(json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": id, "name": step.tool.name(), "input": call_input(step)}
            ]}}));
            if let Some(text) = result_for(step) {
                lines.push(json!({"type": "user", "message": {"content": [
                    {"type": "tool_result", "tool_use_id": id, "content": text}
                ]}}));
            }
        }
        lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn judged(steps: &[ProbeStep], jsonl: &str, init: &Value) -> ProbeReport {
        evaluate(steps, jsonl, Some(init), &TOOLS)
    }

    fn verdict<'a>(report: &'a ProbeReport, id: &str) -> &'a Verdict {
        &report
            .steps
            .iter()
            .find(|step| step.id == id)
            .unwrap_or_else(|| panic!("no step {id}"))
            .verdict
    }

    fn assert_only_failing(report: &ProbeReport, id: &str) {
        assert!(!report.passed);
        for step in &report.steps {
            if step.id != id {
                assert_eq!(step.verdict, Verdict::Pass, "{}", step.id);
            }
        }
    }

    #[test]
    fn passes_on_the_measured_results() {
        let steps = plan();
        let jsonl = transcript(&steps, |step| Some(measured(step)));
        let report = judged(&steps, &jsonl, &good_init());
        assert!(report.passed, "{}", report.render());
    }

    #[test]
    fn a_result_given_as_text_blocks_is_read() {
        let steps = plan();
        let jsonl = transcript(&steps, |step| Some(measured(step))).replace(
            "\"content\":\"absent\"",
            "\"content\":[{\"type\":\"text\",\"text\":\"absent\"}]",
        );
        assert!(judged(&steps, &jsonl, &good_init()).passed);
    }

    #[test]
    fn a_harness_that_ignored_the_sandbox_fails_the_write_step() {
        let steps = plan();
        for silent in ["", "ok"] {
            let jsonl = transcript(&steps, |step| {
                Some(if step.id == "tmp-write" {
                    silent.to_string()
                } else {
                    measured(step)
                })
            });
            let report = judged(&steps, &jsonl, &good_init());
            assert!(matches!(verdict(&report, "tmp-write"), Verdict::Fail(_)));
            assert_only_failing(&report, "tmp-write");
        }
    }

    #[test]
    fn a_skipped_step_is_not_run() {
        let steps = plan();
        let jsonl = transcript(&steps, |step| {
            (step.id != "network").then(|| measured(step))
        });
        let report = judged(&steps, &jsonl, &good_init());
        assert_eq!(verdict(&report, "network"), &Verdict::NotRun);
        assert_only_failing(&report, "network");
    }

    #[test]
    fn a_call_without_a_result_is_not_run() {
        let steps = plan();
        let jsonl = transcript(&steps, |step| (step.id != "git").then(|| measured(step)));
        let report = judged(&steps, &jsonl, &good_init());
        assert_eq!(verdict(&report, "git"), &Verdict::NotRun);
    }

    #[test]
    fn an_altered_command_is_not_run() {
        let steps = plan();
        let mut altered = steps.clone();
        for step in &mut altered {
            if step.id == "home-write" {
                step.input.push_str(" 2>&1");
            }
        }
        let jsonl = transcript(&altered, |step| Some(measured(step)));
        let report = judged(&steps, &jsonl, &good_init());
        assert_eq!(verdict(&report, "home-write"), &Verdict::NotRun);
        assert_only_failing(&report, "home-write");
    }

    #[test]
    fn a_grep_on_another_path_is_not_run() {
        let steps = plan();
        let mut altered = steps.clone();
        for step in &mut altered {
            if step.id == "fixture-grep" {
                step.input = "relais-probe\t/elsewhere".to_string();
            }
        }
        let jsonl = transcript(&altered, |step| Some(measured(step)));
        let report = judged(&steps, &jsonl, &good_init());
        assert_eq!(verdict(&report, "fixture-grep"), &Verdict::NotRun);
    }

    #[test]
    fn a_readable_fixture_fails_the_step() {
        let steps = plan();
        let jsonl = transcript(&steps, |step| {
            Some(if step.id == "fixture-bash" {
                "READABLE".to_string()
            } else {
                measured(step)
            })
        });
        let report = judged(&steps, &jsonl, &good_init());
        assert!(matches!(verdict(&report, "fixture-bash"), Verdict::Fail(_)));
        assert_only_failing(&report, "fixture-bash");
    }

    #[test]
    fn a_present_credential_fails_the_step() {
        let steps = plan();
        for id in ["auth-env", "synthetic-env"] {
            let jsonl = transcript(&steps, |step| {
                Some(if step.id == id {
                    "PRESENT\nabsent".to_string()
                } else {
                    measured(step)
                })
            });
            let report = judged(&steps, &jsonl, &good_init());
            assert!(matches!(verdict(&report, id), Verdict::Fail(_)));
            assert_only_failing(&report, id);
        }
    }

    #[test]
    fn a_permitted_read_fails_the_step() {
        let steps = plan();
        let jsonl = transcript(&steps, |step| {
            Some(if step.id == "fixture-read" {
                "secret contents".to_string()
            } else {
                measured(step)
            })
        });
        let report = judged(&steps, &jsonl, &good_init());
        assert_only_failing(&report, "fixture-read");
    }

    #[test]
    fn a_refusal_is_not_output() {
        let steps = plan();
        let jsonl = transcript(&steps, |step| {
            Some(if step.id == "pipe" {
                "denied by your permission settings".to_string()
            } else {
                measured(step)
            })
        });
        assert_only_failing(&judged(&steps, &jsonl, &good_init()), "pipe");
    }

    /// The plan's transcript with `id`'s result replaced by `text`.
    fn with_result(id: &str, text: &str) -> (Vec<ProbeStep>, ProbeReport) {
        let steps = plan();
        let jsonl = transcript(&steps, |step| {
            Some(if step.id == id {
                text.to_string()
            } else {
                measured(step)
            })
        });
        let report = judged(&steps, &jsonl, &good_init());
        (steps, report)
    }

    #[test]
    fn a_failed_command_never_passes_a_positive_control() {
        let failures = [
            (
                "pipe",
                "Exit code 1\n(eval):1: operation not permitted: /var/x/relais-probe-pipe",
            ),
            ("git", "touch: relais-probe-git: Operation not permitted"),
            (
                "git",
                "fatal: pathspec 'relais-probe-git' did not match any files",
            ),
            ("pipe", "a b"),
            // The expected line is there, but so is a denial or a failure:
            // only the guard, not the line match, fails these.
            (
                "pipe",
                "a\n(eval):1: operation not permitted: /var/x/relais-probe-pipe",
            ),
            ("pipe", "a\nExit code 1"),
            ("git", " A relais-probe-git\nfatal: Operation not permitted"),
        ];
        for (id, text) in failures {
            let (_, report) = with_result(id, text);
            assert!(matches!(verdict(&report, id), Verdict::Fail(_)), "{id}");
            assert_only_failing(&report, id);
        }
    }

    #[test]
    fn the_git_step_passes_on_a_status_line_for_the_file() {
        let (_, report) = with_result("git", " M .claude/settings.json\n A relais-probe-git");
        assert!(report.passed, "{}", report.render());
        let (_, report) = with_result("git", "AN relais-probe-git");
        assert!(report.passed, "{}", report.render());
    }

    fn assert_malformed(jsonl: &str) {
        let steps = plan();
        let report = judged(&steps, jsonl, &good_init());
        assert!(!report.passed);
        for step in &report.steps {
            match &step.verdict {
                Verdict::Fail(why) => assert!(why.contains("malformed"), "{why}"),
                other => panic!("{} was {other:?}", step.id),
            }
        }
    }

    #[test]
    fn a_line_that_is_not_an_object_fails_the_report() {
        assert_malformed("5");
    }

    #[test]
    fn a_tool_use_without_id_or_name_fails_the_report() {
        for item in [
            json!({"type": "tool_use", "name": "Bash", "input": {}}),
            json!({"type": "tool_use", "id": "t", "input": {}}),
        ] {
            assert_malformed(&json!({"message": {"content": [item]}}).to_string());
        }
    }

    #[test]
    fn a_tool_result_without_an_id_or_readable_content_fails_the_report() {
        for item in [
            json!({"type": "tool_result", "content": "x"}),
            json!({"type": "tool_result", "tool_use_id": "t", "content": 7}),
            json!({"type": "tool_result", "tool_use_id": "t"}),
            json!({"type": "tool_result", "tool_use_id": "t", "content": [{"type": "image"}]}),
        ] {
            assert_malformed(&json!({"message": {"content": [item]}}).to_string());
        }
    }

    fn assert_init_fails(init: Option<&Value>, mentions: &str) {
        let steps = plan();
        let jsonl = transcript(&steps, |step| Some(measured(step)));
        let report = evaluate(&steps, &jsonl, init, &TOOLS);
        assert!(!report.passed);
        assert!(report.steps.iter().all(|s| s.verdict == Verdict::Pass));
        let why = report.init.expect_err("init must fail");
        assert!(why.contains(mentions), "{why}");
    }

    #[test]
    fn an_init_with_an_mcp_server_fails() {
        let mut init = good_init();
        init["mcp_servers"] = json!([{"name": "x", "status": "connected"}]);
        assert_init_fails(Some(&init), "MCP");
    }

    #[test]
    fn an_init_with_a_foreign_plugin_fails() {
        let mut init = good_init();
        init["plugins"] = json!([{"name": "p", "source": "p@market"}]);
        assert_init_fails(Some(&init), "p@market");
    }

    #[test]
    fn an_init_with_an_extra_or_missing_tool_fails() {
        let mut init = good_init();
        init["tools"] = json!(["Bash", "Read", "Edit", "Write", "Grep", "Glob", "WebFetch"]);
        assert_init_fails(Some(&init), "extra [WebFetch]");
        init["tools"] = json!(["Bash", "Read", "Edit", "Write", "Grep"]);
        assert_init_fails(Some(&init), "missing [Glob]");
    }

    #[test]
    fn a_missing_init_fails() {
        assert_init_fails(None, "no init record");
    }

    #[test]
    fn tool_order_in_init_does_not_matter() {
        let mut init = good_init();
        init["tools"] = json!(["Glob", "Grep", "Write", "Edit", "Read", "Bash"]);
        let steps = plan();
        let jsonl = transcript(&steps, |step| Some(measured(step)));
        assert!(judged(&steps, &jsonl, &init).passed);
    }

    #[test]
    fn a_malformed_line_fails_the_whole_report() {
        let steps = plan();
        let jsonl = format!(
            "{}\n{{not json",
            transcript(&steps, |step| Some(measured(step)))
        );
        let report = judged(&steps, &jsonl, &good_init());
        assert!(!report.passed);
        for step in &report.steps {
            match &step.verdict {
                Verdict::Fail(why) => assert!(why.contains("malformed"), "{why}"),
                other => panic!("{} was {other:?}", step.id),
            }
        }
    }

    #[test]
    fn the_report_renders_a_line_per_step_within_80_columns() {
        let steps = plan();
        let jsonl = transcript(&steps, |step| match step.id {
            "git" => None,
            "tmp-write" => Some("x".repeat(300)),
            _ => Some(measured(step)),
        });
        let text = judged(&steps, &jsonl, &good_init()).render();
        assert_eq!(text.lines().count(), steps.len() + 1);
        assert!(text.lines().all(|line| line.chars().count() <= 80));
        assert!(text.contains("✓ pipe: ok"));
        assert!(text.contains("– git: not run"));
        assert!(text.contains("✗ tmp-write: "));
        assert!(text.contains("✓ init: ok"));
    }

    #[test]
    fn the_plan_is_the_listed_steps_built_from_its_inputs() {
        let steps = plan();
        let ids: Vec<_> = steps.iter().map(|step| step.id).collect();
        assert_eq!(
            ids,
            [
                "pipe",
                "tmp-write",
                "home-write",
                "network",
                "fixture-bash",
                "synthetic-env",
                "auth-env",
                "git",
                "excluded-python",
                "fixture-read",
                "fixture-grep"
            ]
        );
        let input = |id: &str| steps.iter().find(|s| s.id == id).unwrap().input.clone();
        assert_eq!(input("tmp-write"), "touch /tmp/relais-probe-n1");
        assert_eq!(input("home-write"), "touch '/home/u/relais-probe-n1'");
        assert_eq!(
            input("fixture-bash"),
            "test -r '/fx/fixtures/secret.txt' && echo READABLE || echo denied"
        );
        assert_eq!(
            input("excluded-python"),
            "python3 -c 'open(\"/home/u/relais-probe-py-n1\",\"w\")'"
        );
        assert_eq!(input("fixture-read"), "/fx/fixtures/secret.txt");
        assert_eq!(input("fixture-grep"), "relais-probe\t/fx/fixtures");
    }

    #[test]
    fn every_env_step_asks_for_presence_and_never_a_value() {
        let steps = plan();
        for id in ["synthetic-env", "auth-env"] {
            let input = &steps.iter().find(|s| s.id == id).unwrap().input;
            for command in input.split("; ") {
                assert!(command.starts_with("printenv "), "{command}");
                assert!(command.ends_with(PRESENCE), "{command}");
            }
        }
        let allowlist = probe_plan_allowlist("n1");
        let ids: Vec<_> = allowlist.iter().map(|step| step.id).collect();
        assert_eq!(ids, ["scrub-env", "auth-env"]);
        for step in &allowlist {
            for command in step.input.split("; ") {
                assert!(command.starts_with("printenv "), "{command}");
                assert!(command.ends_with(PRESENCE), "{command}");
            }
            assert_eq!(step.expect, line("absent"));
        }
        let plan_auth = steps.iter().find(|s| s.id == "auth-env").unwrap();
        assert_eq!(&allowlist[1], plan_auth);
    }

    #[test]
    fn a_path_with_a_space_is_one_shell_word() {
        let steps = plan_for("/fx dir/secret.txt", "/home/my user").unwrap();
        let input = |id: &str| steps.iter().find(|s| s.id == id).unwrap().input.clone();
        assert!(input("fixture-bash").starts_with("test -r '/fx dir/secret.txt' &&"));
        assert_eq!(input("home-write"), "touch '/home/my user/relais-probe-n1'");
    }

    #[test]
    fn a_single_quote_in_a_path_is_escaped_for_the_shell() {
        let steps = plan_for("/fx/it's.txt", "/home/u").unwrap();
        let bash = steps.iter().find(|s| s.id == "fixture-bash").unwrap();
        assert!(bash.input.starts_with(r"test -r '/fx/it'\''s.txt' &&"));
    }

    #[test]
    fn a_path_with_a_newline_is_refused() {
        assert!(plan_for("/fx/a\nb", "/home/u").is_err());
        assert!(plan_for("/fx/a", "/home/u\nx").is_err());
    }

    #[test]
    fn a_home_the_python_step_cannot_quote_is_refused() {
        assert!(plan_for("/fx/a", "/home/\"u").is_err());
        assert!(plan_for("/fx/a", "/home/u\\x").is_err());
    }

    #[test]
    fn a_positive_control_whose_result_is_an_error_fails() {
        let steps = plan();
        let jsonl = transcript(&steps, |step| Some(measured(step)))
            .replace("\"content\":\"a\"", "\"content\":\"a\",\"is_error\":true");
        assert!(jsonl.contains("\"is_error\":true"));
        let report = judged(&steps, &jsonl, &good_init());
        assert!(matches!(verdict(&report, "pipe"), Verdict::Fail(_)));
        assert_only_failing(&report, "pipe");
    }

    #[test]
    fn a_denied_command_with_is_error_still_passes_its_negative_control() {
        let steps = plan();
        let jsonl = transcript(&steps, |step| Some(measured(step))).replace(
            "\"tool_use_id\":\"toolu_tmp-write\"",
            "\"tool_use_id\":\"toolu_tmp-write\",\"is_error\":true",
        );
        assert!(jsonl.contains("\"is_error\":true"));
        assert!(judged(&steps, &jsonl, &good_init()).passed);
    }

    #[test]
    fn a_violation_for_another_host_fails_the_network_step() {
        let (_, report) = with_result(
            "network",
            "Exit code 56\n<sandbox_violations>\ndeny network-outbound \
             evil.test:443\n</sandbox_violations>",
        );
        assert!(matches!(verdict(&report, "network"), Verdict::Fail(_)));
        assert_only_failing(&report, "network");
        // curl names the probe host in its own error, but the harness
        // denied another host: still a failure.
        let (_, report) = with_result(
            "network",
            "Exit code 6\ncurl: (6) Could not resolve host: example.com\n\
             <sandbox_violations>\ndeny network-outbound evil.test:443\n</sandbox_violations>",
        );
        assert!(matches!(verdict(&report, "network"), Verdict::Fail(_)));
    }

    #[test]
    fn an_init_without_plugins_fails() {
        let mut init = good_init();
        init.as_object_mut().unwrap().remove("plugins");
        assert_init_fails(Some(&init), "plugins missing");
        init["plugins"] = json!("none");
        assert_init_fails(Some(&init), "plugins missing");
    }

    #[test]
    fn the_prompt_numbers_every_step_with_its_exact_input() {
        let steps = plan();
        let prompt = probe_prompt(&steps);
        assert!(prompt.contains("exactly one tool call"));
        assert!(prompt.contains("do not work around"));
        for (n, step) in steps.iter().enumerate() {
            let shown = step.input.replace('\t', " path: ");
            assert!(prompt.contains(&format!("\n{}. ", n + 1)), "{n}");
            assert!(prompt.contains(&shown) || step.tool == ProbeTool::Grep);
        }
        assert!(prompt.contains("pattern: relais-probe path: /fx/fixtures"));
    }
}
