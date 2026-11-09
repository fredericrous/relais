//! Loop detection for a native worker's transcript (pure, no I/O).
//!
//! A worker that repeats the same failing Bash command with no edit between
//! is looping. [`LoopGuard`] consumes transcript text as it grows and says
//! when one failure has been seen [`DEFAULT_LOOP_THRESHOLD`] (or the chosen
//! number of) times. Progress is judged from the transcript alone: an edit
//! tool use, or a Bash command that mutates files, clears every count.

use std::collections::HashMap;
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::orchestration::transcript::{parse_tool_events, ToolEvent, ToolOutcome};
use crate::router::capture::redact;

pub const DEFAULT_LOOP_THRESHOLD: u32 = 3;
pub const MIN_LOOP_THRESHOLD: u32 = crate::policy::MIN_LOOP_THRESHOLD;

/// A threshold below [`MIN_LOOP_THRESHOLD`] was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThresholdTooLow {
    pub given: u32,
}

impl fmt::Display for ThresholdTooLow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "loop threshold {} is below the minimum {MIN_LOOP_THRESHOLD}",
            self.given
        )
    }
}

impl std::error::Error for ThresholdTooLow {}

/// The longest command the stderr event line shows, in characters.
pub const SHOWN_COMMAND_CHARS: usize = 60;

/// `text` with known secret shapes redacted, on one line: whitespace runs
/// (newlines included) become one space and other control characters are
/// escaped.
pub fn redacted_one_line(text: &str) -> String {
    let (redacted, _) = redact(text);
    let mut line = String::with_capacity(redacted.len());
    let mut in_gap = false;
    for ch in redacted.trim().chars() {
        if ch.is_whitespace() {
            if !in_gap {
                line.push(' ');
            }
            in_gap = true;
            continue;
        }
        in_gap = false;
        if ch.is_control() {
            line.extend(ch.escape_default());
        } else {
            line.push(ch);
        }
    }
    line
}

/// `line` cut to `SHOWN_COMMAND_CHARS` characters (Unicode scalar values),
/// the last one `…`, when it is longer.
pub fn shown(line: &str) -> String {
    if line.chars().count() <= SHOWN_COMMAND_CHARS {
        return line.to_string();
    }
    let mut cut: String = line.chars().take(SHOWN_COMMAND_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// The loop a guard found: one fingerprint reached the threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopFired {
    pub command: String,
    /// What repeated: the first error line, or a hash of the whole output.
    pub evidence: Evidence,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Evidence {
    ErrorLine(String),
    OutputHash(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Fingerprint {
    command: String,
    evidence: Evidence,
}

#[derive(Debug)]
struct PendingCall {
    command: String,
    ends_in_output_filter: bool,
}

#[derive(Debug)]
pub struct LoopGuard {
    threshold: u32,
    partial_line: String,
    pending: HashMap<String, PendingCall>,
    counts: HashMap<Fingerprint, u32>,
}

impl LoopGuard {
    pub fn new(threshold: u32) -> Result<Self, ThresholdTooLow> {
        if threshold < MIN_LOOP_THRESHOLD {
            return Err(ThresholdTooLow { given: threshold });
        }
        Ok(Self {
            threshold,
            partial_line: String::new(),
            pending: HashMap::new(),
            counts: HashMap::new(),
        })
    }

    pub fn with_default_threshold() -> Self {
        Self {
            threshold: DEFAULT_LOOP_THRESHOLD,
            partial_line: String::new(),
            pending: HashMap::new(),
            counts: HashMap::new(),
        }
    }

    /// Feed the next piece of transcript text. Only complete lines are
    /// read; a partial last line is held until its newline arrives.
    /// Returns the first loop found in this chunk.
    pub fn push_chunk(&mut self, chunk: &str) -> Option<LoopFired> {
        self.partial_line.push_str(chunk);
        let complete_len = self.partial_line.rfind('\n').map(|at| at + 1)?;
        let complete: String = self.partial_line.drain(..complete_len).collect();
        self.observe(parse_tool_events(&complete))
    }

    /// Feed parsed events. Returns the first loop found among them; every
    /// event is still applied, so the state stays consistent.
    pub fn observe(&mut self, events: impl IntoIterator<Item = ToolEvent>) -> Option<LoopFired> {
        let mut first = None;
        for event in events {
            let fired = self.apply(event);
            if first.is_none() {
                first = fired;
            }
        }
        first
    }

    fn apply(&mut self, event: ToolEvent) -> Option<LoopFired> {
        match event {
            ToolEvent::EditUse { .. } => {
                self.counts.clear();
                None
            }
            ToolEvent::BashUse { id, command } => {
                let command = normalise_command(&command);
                let shape = CommandShape::of(&command);
                if shape.mutates {
                    self.counts.clear();
                } else {
                    self.pending.insert(
                        id,
                        PendingCall {
                            command,
                            ends_in_output_filter: shape.ends_in_output_filter,
                        },
                    );
                }
                None
            }
            ToolEvent::ToolResult {
                tool_use_id,
                outcome,
                output,
            } => {
                let call = self.pending.remove(&tool_use_id)?;
                // holds-until: the tool result carries the pipeline's exit status — the pipe hides it, so a trailing tail/head/grep is counted whatever is_error says
                if call.ends_in_output_filter {
                    let evidence = Evidence::OutputHash(hash_of(&output));
                    return self.count(call.command, evidence);
                }
                match outcome {
                    ToolOutcome::Failed {
                        first_line: Some(line),
                    } => self.count(call.command, Evidence::ErrorLine(line)),
                    ToolOutcome::Failed { first_line: None } => {
                        let evidence = Evidence::OutputHash(hash_of(&output));
                        self.count(call.command, evidence)
                    }
                    ToolOutcome::Succeeded => {
                        self.counts
                            .retain(|fingerprint, _| fingerprint.command != call.command);
                        None
                    }
                }
            }
        }
    }

    fn count(&mut self, command: String, evidence: Evidence) -> Option<LoopFired> {
        let seen = self
            .counts
            .entry(Fingerprint {
                command: command.clone(),
                evidence: evidence.clone(),
            })
            .or_insert(0);
        *seen += 1;
        (*seen >= self.threshold).then_some(LoopFired {
            command,
            evidence,
            count: *seen,
        })
    }
}

fn hash_of(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Trim, and collapse whitespace outside quotes to one space (a run that
/// holds a newline becomes one newline, which separates commands).
fn normalise_command(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut gap: Option<bool> = None;
    for ch in command.trim().chars() {
        if quote.is_none() && !escaped && ch.is_whitespace() {
            gap = Some(gap.unwrap_or(false) || ch == '\n');
            continue;
        }
        if let Some(has_newline) = gap.take() {
            out.push(if has_newline { '\n' } else { ' ' });
        }
        out.push(ch);
        if escaped {
            escaped = false;
        } else if ch == '\\' && quote != Some('\'') {
            escaped = true;
        } else if let Some(open) = quote {
            if ch == open {
                quote = None;
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        }
    }
    out
}

/// One simple command of a shell line: its words with redirections taken
/// out.
#[derive(Debug, Default)]
struct Segment {
    words: Vec<String>,
    writes_file: bool,
    after_pipe: bool,
}

#[derive(Clone, Copy)]
enum PendingRedirect {
    ToFile,
    Duplicate,
    Input,
}

fn is_descriptor(target: &str) -> bool {
    target == "-" || target.chars().all(|ch| ch.is_ascii_digit())
}

/// How the next segment is joined to the one just ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Joiner {
    Pipe,
    Sequence,
}

struct SegmentReader {
    segments: Vec<Segment>,
    current: Segment,
    word: String,
    in_word: bool,
    redirect: Option<PendingRedirect>,
}

impl SegmentReader {
    fn flush_word(&mut self) {
        if !self.in_word {
            return;
        }
        let word = std::mem::take(&mut self.word);
        self.in_word = false;
        match self.redirect.take() {
            Some(PendingRedirect::ToFile) => {
                if word != "/dev/null" {
                    self.current.writes_file = true;
                }
            }
            Some(PendingRedirect::Duplicate) => {
                if word != "/dev/null" && !is_descriptor(&word) {
                    self.current.writes_file = true;
                }
            }
            Some(PendingRedirect::Input) => {}
            None => self.current.words.push(word),
        }
    }

    fn end_segment(&mut self, joiner: Joiner) {
        self.flush_word();
        self.redirect = None;
        let next = Segment {
            after_pipe: joiner == Joiner::Pipe,
            ..Segment::default()
        };
        self.segments
            .push(std::mem::replace(&mut self.current, next));
    }

    fn push_char(&mut self, ch: char) {
        self.word.push(ch);
        self.in_word = true;
    }
}

/// The heredoc delimiter that starts at `at`, and the position after it.
fn read_heredoc_delimiter(chars: &[char], mut at: usize) -> (String, usize) {
    if chars.get(at) == Some(&'-') {
        at += 1;
    }
    while matches!(chars.get(at), Some(' ' | '\t')) {
        at += 1;
    }
    let mut delimiter = String::new();
    let mut quote: Option<char> = None;
    while let Some(&ch) = chars.get(at) {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => delimiter.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch.is_whitespace() || matches!(ch, ';' | '&' | '|' | '<' | '>') => break,
            None => delimiter.push(ch),
        }
        at += 1;
    }
    (delimiter, at)
}

/// Skip the lines of a heredoc body, through its delimiter line. Returns
/// the position after it.
fn skip_heredoc_body(chars: &[char], mut at: usize, delimiter: &str) -> usize {
    while at < chars.len() {
        let line_end = chars[at..]
            .iter()
            .position(|&ch| ch == '\n')
            .map_or(chars.len(), |offset| at + offset);
        let line: String = chars[at..line_end].iter().collect();
        at = (line_end + 1).min(chars.len());
        if line.trim() == delimiter {
            break;
        }
    }
    at
}

fn read_segments(command: &str) -> Vec<Segment> {
    let chars: Vec<char> = command.chars().collect();
    let mut reader = SegmentReader {
        segments: Vec::new(),
        current: Segment::default(),
        word: String::new(),
        in_word: false,
        redirect: None,
    };
    let mut heredocs: Vec<String> = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let ch = chars[at];
        let next = chars.get(at + 1).copied();
        at += 1;
        match ch {
            '\'' => {
                reader.in_word = true;
                while at < chars.len() && chars[at] != '\'' {
                    reader.word.push(chars[at]);
                    at += 1;
                }
                at += 1;
            }
            '"' => {
                reader.in_word = true;
                while at < chars.len() && chars[at] != '"' {
                    if chars[at] == '\\' && at + 1 < chars.len() {
                        at += 1;
                    }
                    reader.word.push(chars[at]);
                    at += 1;
                }
                at += 1;
            }
            '\\' => {
                if let Some(escaped) = next {
                    reader.push_char(escaped);
                    at += 1;
                }
            }
            '\n' => {
                reader.end_segment(Joiner::Sequence);
                for delimiter in heredocs.drain(..) {
                    at = skip_heredoc_body(&chars, at, &delimiter);
                }
            }
            ';' => reader.end_segment(Joiner::Sequence),
            ' ' | '\t' => reader.flush_word(),
            '|' => {
                let mut joiner = Joiner::Pipe;
                if next == Some('|') {
                    joiner = Joiner::Sequence;
                    at += 1;
                } else if next == Some('&') {
                    at += 1;
                }
                reader.end_segment(joiner);
            }
            '&' if next == Some('&') => {
                at += 1;
                reader.end_segment(Joiner::Sequence);
            }
            '&' if next == Some('>') => {
                reader.flush_word();
                at += 1;
                if chars.get(at) == Some(&'>') {
                    at += 1;
                }
                reader.redirect = Some(PendingRedirect::ToFile);
            }
            '&' => reader.end_segment(Joiner::Sequence),
            '>' => {
                if reader.in_word && reader.word.chars().all(|digit| digit.is_ascii_digit()) {
                    reader.word.clear();
                    reader.in_word = false;
                } else {
                    reader.flush_word();
                }
                if chars.get(at) == Some(&'>') {
                    at += 1;
                }
                reader.redirect = Some(if chars.get(at) == Some(&'&') {
                    at += 1;
                    PendingRedirect::Duplicate
                } else {
                    PendingRedirect::ToFile
                });
            }
            '#' if !reader.in_word => {
                while at < chars.len() && chars[at] != '\n' {
                    at += 1;
                }
            }
            '<' => {
                reader.flush_word();
                let mut markers = 0;
                while chars.get(at) == Some(&'<') {
                    at += 1;
                    markers += 1;
                }
                if markers == 1 {
                    let (delimiter, after) = read_heredoc_delimiter(&chars, at);
                    at = after;
                    if !delimiter.is_empty() {
                        heredocs.push(delimiter);
                    }
                } else {
                    reader.redirect = Some(PendingRedirect::Input);
                }
            }
            _ => reader.push_char(ch),
        }
    }
    reader.end_segment(Joiner::Sequence);
    reader
        .segments
        .into_iter()
        .filter(|segment| !segment.words.is_empty() || segment.writes_file)
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
struct CommandShape {
    mutates: bool,
    ends_in_output_filter: bool,
}

impl CommandShape {
    fn of(command: &str) -> Self {
        let segments = read_segments(command);
        let mutates = segments
            .iter()
            .any(|segment| segment.writes_file || segment_mutates(&words_of_command(segment)));
        let ends_in_output_filter = segments.last().is_some_and(|segment| {
            segment.after_pipe
                && matches!(
                    words_of_command(segment).first().copied(),
                    Some("tail" | "head" | "grep")
                )
        });
        Self {
            mutates,
            ends_in_output_filter,
        }
    }
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

/// The command and its arguments, past leading assignments and wrappers.
fn words_of_command(segment: &Segment) -> Vec<&str> {
    segment
        .words
        .iter()
        .map(String::as_str)
        .skip_while(|word| {
            is_assignment(word) || matches!(*word, "sudo" | "env" | "command" | "nohup" | "time")
        })
        .collect()
}

/// The first word of `args` that is not an option, skipping the values of
/// the options named in `with_value`.
fn subcommand<'a>(args: &[&'a str], with_value: &[&str]) -> Option<&'a str> {
    let mut args = args.iter().copied();
    while let Some(arg) = args.next() {
        if with_value.contains(&arg) {
            args.next();
        } else if !arg.starts_with('-') && !arg.starts_with('+') {
            return Some(arg);
        }
    }
    None
}

// holds-until: progress is judged from the worktree and not from the
// transcript. Only the commands listed here count as changing files, so a
// script that writes files, or a command missing from the list, leaves the
// counts standing and the guard can fire although the worker made progress.
fn segment_mutates(words: &[&str]) -> bool {
    let Some((&name, args)) = words.split_first() else {
        return false;
    };
    match name {
        "rm" | "mv" | "cp" | "touch" | "tee" | "patch" => true,
        "sed" => args.iter().any(|arg| {
            arg.starts_with("--in-place") || (is_short_cluster(arg) && arg.contains('i'))
        }),
        "perl" => args.iter().any(|arg| is_perl_in_place(arg)),
        "git" => matches!(
            subcommand(args, &["-C", "-c"]),
            Some(
                "commit"
                    | "add"
                    | "checkout"
                    | "apply"
                    | "stash"
                    | "reset"
                    | "restore"
                    | "merge"
                    | "rebase"
                    | "cherry-pick"
                    | "switch"
                    | "rm"
                    | "mv"
            )
        ),
        "cargo" => match subcommand(args, &[]) {
            Some("fmt") => !args.contains(&"--check"),
            Some("install" | "add") => true,
            _ => false,
        },
        "npm" | "pnpm" => matches!(subcommand(args, &[]), Some("install" | "add")),
        _ => false,
    }
}

fn is_short_cluster(arg: &str) -> bool {
    arg.len() > 1 && arg.starts_with('-') && !arg.starts_with("--")
}

fn is_perl_in_place(arg: &str) -> bool {
    if !is_short_cluster(arg) {
        return false;
    }
    let flags = &arg[1..];
    match flags.find('i') {
        Some(at) => flags[..at].chars().all(|flag| "pnalwsT0".contains(flag)),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash_use(id: &str, command: &str) -> String {
        json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","id":id,"name":"Bash","input":{"command":command}}
        ]}})
        .to_string()
    }

    fn edit_use(id: &str) -> String {
        json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","id":id,"name":"Edit","input":{"file_path":"/x"}}
        ]}})
        .to_string()
    }

    fn result(id: &str, is_error: bool, text: &str) -> String {
        json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":id,"is_error":is_error,"content":text}
        ]}})
        .to_string()
    }

    fn feed(guard: &mut LoopGuard, lines: &[String]) -> Option<LoopFired> {
        guard.push_chunk(&format!("{}\n", lines.join("\n")))
    }

    fn failing_run(
        guard: &mut LoopGuard,
        id: &str,
        command: &str,
        text: &str,
    ) -> Option<LoopFired> {
        feed(guard, &[bash_use(id, command), result(id, true, text)])
    }

    fn guard() -> LoopGuard {
        LoopGuard::with_default_threshold()
    }

    #[test]
    fn three_identical_failures_fire() {
        let mut guard = guard();
        assert_eq!(
            failing_run(&mut guard, "a", "make test", "boom\nmore"),
            None
        );
        assert_eq!(
            failing_run(&mut guard, "b", "make test", "boom\nother"),
            None
        );
        assert_eq!(
            failing_run(&mut guard, "c", "make test", "boom"),
            Some(LoopFired {
                command: "make test".into(),
                evidence: Evidence::ErrorLine("boom".into()),
                count: 3
            })
        );
    }

    #[test]
    fn whitespace_differences_are_one_command() {
        let mut guard = guard();
        failing_run(&mut guard, "a", "make   test", "boom");
        failing_run(&mut guard, "b", " make test ", "boom");
        assert!(failing_run(&mut guard, "c", "make\ttest", "boom").is_some());
    }

    #[test]
    fn different_error_lines_are_different_fingerprints() {
        let mut guard = guard();
        failing_run(&mut guard, "a", "make test", "one");
        failing_run(&mut guard, "b", "make test", "two");
        assert_eq!(failing_run(&mut guard, "c", "make test", "three"), None);
    }

    #[test]
    fn an_edit_between_clears() {
        let mut guard = guard();
        failing_run(&mut guard, "a", "make test", "boom");
        failing_run(&mut guard, "b", "make test", "boom");
        feed(&mut guard, &[edit_use("e")]);
        assert_eq!(failing_run(&mut guard, "c", "make test", "boom"), None);
    }

    #[test]
    fn a_mutating_bash_clears() {
        for mutating in [
            "sed -i 's/a/b/' f",
            "perl -pi -e 's/a/b/' f",
            "rm -f x",
            "git commit -m x",
            "git -C /r add .",
            "cargo fmt",
            "pnpm add left-pad",
            "echo hi > out.txt",
            "echo hi >> out.txt",
            "make && touch done",
        ] {
            let mut guard = guard();
            failing_run(&mut guard, "a", "make test", "boom");
            failing_run(&mut guard, "b", "make test", "boom");
            feed(
                &mut guard,
                &[bash_use("m", mutating), result("m", false, "")],
            );
            assert_eq!(
                failing_run(&mut guard, "c", "make test", "boom"),
                None,
                "{mutating}"
            );
        }
    }

    #[test]
    fn redirects_to_a_stream_or_null_do_not_clear() {
        for harmless in [
            "ls 2>&1",
            "ls >/dev/null",
            "ls 2>/dev/null",
            "ls &>/dev/null",
            "ls >&2",
            "grep 'a>b' f",
            "echo 'rm -rf x'",
            "git status",
            "git log --stat",
            "cargo fmt --check",
            "sed -n 1p f",
        ] {
            let mut guard = guard();
            failing_run(&mut guard, "a", "make test", "boom");
            failing_run(&mut guard, "b", "make test", "boom");
            feed(
                &mut guard,
                &[bash_use("m", harmless), result("m", true, "x")],
            );
            assert!(
                failing_run(&mut guard, "c", "make test", "boom").is_some(),
                "{harmless}"
            );
        }
    }

    #[test]
    fn interleaved_different_commands_still_count() {
        let mut guard = guard();
        failing_run(&mut guard, "a", "make test", "boom");
        failing_run(&mut guard, "x", "ls missing", "no such file");
        failing_run(&mut guard, "b", "make test", "boom");
        failing_run(&mut guard, "y", "cat missing", "no such file");
        assert!(failing_run(&mut guard, "c", "make test", "boom").is_some());
    }

    #[test]
    fn a_success_clears_only_that_command() {
        let mut guard = guard();
        failing_run(&mut guard, "a", "make test", "boom");
        failing_run(&mut guard, "x", "make lint", "bad");
        failing_run(&mut guard, "b", "make test", "boom");
        failing_run(&mut guard, "y", "make lint", "bad");
        feed(
            &mut guard,
            &[bash_use("ok", "make lint"), result("ok", false, "fine")],
        );
        assert!(failing_run(&mut guard, "c", "make test", "boom").is_some());
        assert_eq!(failing_run(&mut guard, "z", "make lint", "bad"), None);
    }

    #[test]
    fn a_piped_tail_command_fires_on_the_same_output() {
        let mut guard = guard();
        let command = "cargo test 2>&1 | tail -40";
        let output = "test result: FAILED. 1 failed";
        assert_eq!(failing_run(&mut guard, "a", command, output), None);
        assert_eq!(failing_run(&mut guard, "b", command, output), None);
        let fired = failing_run(&mut guard, "c", command, output).expect("fires");
        assert_eq!(fired.count, 3);
        assert!(matches!(fired.evidence, Evidence::OutputHash(_)));
    }

    #[test]
    fn a_piped_tail_command_is_counted_whatever_is_error_says() {
        let mut guard = guard();
        let command = "cargo test | tail -5";
        for id in ["a", "b"] {
            feed(
                &mut guard,
                &[bash_use(id, command), result(id, false, "FAILED")],
            );
        }
        assert!(feed(
            &mut guard,
            &[bash_use("c", command), result("c", false, "FAILED")]
        )
        .is_some());
    }

    #[test]
    fn only_a_pipe_joins_a_trailing_output_filter() {
        let filter_of = |command: &str| CommandShape::of(command).ends_in_output_filter;
        assert!(filter_of("cargo test | tail -5"));
        assert!(filter_of("cargo test |& grep error"));
        assert!(!filter_of("cargo test || tail -5"));
        assert!(!filter_of("cargo test && tail -5"));
        assert!(!filter_of("cargo test; head -5 log"));
    }

    #[test]
    fn a_piped_tail_command_with_changing_output_does_not_fire() {
        let mut guard = guard();
        let command = "cargo test 2>&1 | tail -40";
        failing_run(&mut guard, "a", command, "took 1s");
        failing_run(&mut guard, "b", command, "took 2s");
        assert_eq!(failing_run(&mut guard, "c", command, "took 3s"), None);
    }

    #[test]
    fn a_heredoc_body_is_not_read_as_a_redirect() {
        let command = "python3 - <<'EOF'\nprint(a > b)\nEOF";
        let mut guard = guard();
        failing_run(&mut guard, "a", command, "boom");
        failing_run(&mut guard, "b", command, "boom");
        assert!(failing_run(&mut guard, "c", command, "boom").is_some());
    }

    #[test]
    fn a_comment_is_not_read_as_a_redirect() {
        let command = "make test # > out";
        let mut guard = guard();
        failing_run(&mut guard, "a", command, "boom");
        failing_run(&mut guard, "b", command, "boom");
        assert!(failing_run(&mut guard, "c", command, "boom").is_some());
    }

    #[test]
    fn a_failure_with_empty_output_is_fingerprinted_by_hash() {
        let mut guard = guard();
        failing_run(&mut guard, "a", "make", "");
        failing_run(&mut guard, "b", "make", "");
        let fired = failing_run(&mut guard, "c", "make", "").expect("fires");
        assert_eq!(fired.count, 3);
        assert!(matches!(fired.evidence, Evidence::OutputHash(_)));
    }

    #[test]
    fn a_threshold_below_two_is_rejected() {
        assert_eq!(LoopGuard::new(1).unwrap_err(), ThresholdTooLow { given: 1 });
        assert!(LoopGuard::new(0).is_err());
        assert!(LoopGuard::new(2).is_ok());
    }

    #[test]
    fn a_threshold_of_two_fires_on_the_second_failure() {
        let mut guard = LoopGuard::new(2).expect("valid");
        assert_eq!(failing_run(&mut guard, "a", "make", "boom"), None);
        assert!(failing_run(&mut guard, "b", "make", "boom").is_some());
    }

    #[test]
    fn malformed_lines_are_ignored() {
        let mut guard = guard();
        let noise = vec![
            "not json".to_string(),
            "{\"type\":\"assistant\"".to_string(),
            "[]".to_string(),
        ];
        assert_eq!(feed(&mut guard, &noise), None);
        failing_run(&mut guard, "a", "make", "boom");
        feed(&mut guard, &noise);
        failing_run(&mut guard, "b", "make", "boom");
        assert!(failing_run(&mut guard, "c", "make", "boom").is_some());
    }

    #[test]
    fn a_partial_last_line_is_held_until_its_newline() {
        let mut guard = LoopGuard::new(2).expect("valid");
        failing_run(&mut guard, "a", "make", "boom");
        let use_line = bash_use("b", "make");
        let result_line = result("b", true, "boom");
        let (head, tail) = result_line.split_at(result_line.len() / 2);
        assert_eq!(guard.push_chunk(&format!("{use_line}\n{head}")), None);
        assert_eq!(guard.push_chunk(tail), None);
        assert!(guard.push_chunk("\n").is_some());
    }

    #[test]
    fn a_result_in_a_later_poll_pairs_with_its_use() {
        let mut guard = LoopGuard::new(2).expect("valid");
        failing_run(&mut guard, "a", "make", "boom");
        assert_eq!(
            guard.push_chunk(&format!("{}\n", bash_use("b", "make"))),
            None
        );
        assert!(guard
            .push_chunk(&format!("{}\n", result("b", true, "boom")))
            .is_some());
    }

    #[test]
    fn a_result_without_a_known_use_is_ignored() {
        let mut guard = LoopGuard::new(2).expect("valid");
        for id in ["x", "y", "z"] {
            assert_eq!(feed(&mut guard, &[result(id, true, "boom")]), None);
        }
    }

    #[test]
    fn normalisation_keeps_quoted_whitespace() {
        assert_eq!(
            normalise_command("  echo   'a   b'  \"c  d\"  "),
            "echo 'a   b' \"c  d\""
        );
    }
}
