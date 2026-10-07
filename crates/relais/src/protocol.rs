//! A run's protocol channel and its events (SPEC §29).
//!
//! Nothing a run does is out of sight: every step is an [`Event`], numbered
//! within its run, appended to `<artifacts>/<run>/events.jsonl` whether or
//! not a reader is attached, and — under `relais run --protocol` — written
//! as one JSON line to the real stdout, which that flag keeps for protocol
//! lines only. Every other byte the process writes, to stdout or to stderr,
//! goes to stderr, and is mirrored into the run's `events.jsonl` as a
//! `stderr` event once the run's artifacts directory is known.
//!
//! A leaf module: it knows no run state, only what an event says.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::money::CostCompleteness;

/// The most text one `output` event carries.
pub const OUTPUT_CHUNK_BYTES: usize = 4 * 1024;

/// The most output text one check puts into events, in all. The rest is
/// counted, never buffered; the full log stays in the artifacts directory.
pub const OUTPUT_BUDGET_BYTES: u64 = 64 * 1024;

/// How often a running check's new output is merged into events.
pub const OUTPUT_MERGE: Duration = Duration::from_millis(100);

/// The most text one `stderr` event carries; a longer line is cut.
const STDERR_TEXT_BYTES: usize = 4 * 1024;

/// The longest stretch read from the pipe as one line, so a stream with no
/// newline in it cannot grow a buffer without limit.
const STDERR_LINE_BYTES: u64 = 64 * 1024;

/// How long [`Channel::close`] waits for the mirror to drain the pipe.
const DRAIN_WAIT: Duration = Duration::from_secs(2);

/// What kind of agent a dispatch is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Worker,
    Reviewer,
    Planner,
}

/// The tokens a dispatch reported; a field the harness did not report is
/// `null`, never zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
}

/// What a figure of money is, and how well it is known: `booked` is
/// micro-USD, `null` when nothing was reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CostFigure {
    pub booked: Option<i64>,
    pub completeness: CostCompleteness,
}

/// One thing a run did.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A state transition, as the ledger records it.
    Phase {
        state: String,
        reason: String,
        detail: serde_json::Value,
    },
    /// A step of the run that is not a state transition: preflight, the
    /// baseline, the task worktree and its setup, the review, the
    /// receipt. A second `step` of the same name updates its detail.
    Step {
        name: String,
        detail: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_attempts: Option<u32>,
    },
    DispatchStarted {
        dispatch: String,
        agent_kind: AgentKind,
        /// The attempt's number, for a worker; reviews and plans have none.
        attempt: Option<u32>,
        model: String,
        effort: Option<String>,
    },
    DispatchEnded {
        dispatch: String,
        /// The agent that ran it, when one was bound: a repair continues
        /// the same agent under a new dispatch, so a reader rebuilding the
        /// timeline can tell one agent from two.
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        outcome: String,
        usage: Option<TokenUsage>,
        cost: Option<CostFigure>,
    },
    /// What one usage event booked.
    Cost {
        booked: Option<i64>,
        completeness: CostCompleteness,
    },
    CheckStarted {
        label: String,
        argv: Vec<String>,
    },
    /// A piece of a running check's output. `elided_bytes` counts what was
    /// left out of the events (the log has it all).
    Output {
        label: String,
        text: String,
        elided_bytes: u64,
    },
    CheckEnded {
        label: String,
        exit: Option<i32>,
        duration_ms: u64,
    },
    /// The run chose to repair, escalate, review or stop.
    Decision {
        what: String,
        reason: String,
    },
    Outcome {
        state: String,
        receipt: Option<String>,
        /// What the run's last candidate changed, when it kept one: the
        /// pane's outcome row, and a timeline rebuilt from the events.
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<Summary>,
    },
    /// A line the process wrote to stdout or stderr under `--protocol`.
    Stderr {
        text: String,
    },
}

impl Event {
    /// Whether a protocol reader is sent this event. It reads the child's
    /// stderr itself, so a `stderr` event goes to `events.jsonl` only.
    fn is_protocol_line(&self) -> bool {
        match self {
            Self::Stderr { .. } => false,
            Self::Phase { .. }
            | Self::Step { .. }
            | Self::DispatchStarted { .. }
            | Self::DispatchEnded { .. }
            | Self::Cost { .. }
            | Self::CheckStarted { .. }
            | Self::Output { .. }
            | Self::CheckEnded { .. }
            | Self::Decision { .. }
            | Self::Outcome { .. } => true,
        }
    }
}

/// The wire form of an event.
#[derive(Serialize)]
struct Line<'a> {
    relais: &'static str,
    run: &'a str,
    /// The event's place among the events a reader of the channel sees:
    /// gapless on stdout and in the file alike. A `stderr` event has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<u64>,
    /// A `stderr` event's own place among the mirrored lines, numbered
    /// apart so they never take a `seq` the channel would skip.
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr_seq: Option<u64>,
    at: String,
    event: &'a Event,
}

/// What the run asks of the relais plugin, and its end: one JSON line each,
/// written through the same [`Wire`] as the events.
#[derive(Debug, Serialize)]
#[serde(tag = "relais", rename_all = "snake_case")]
pub enum Request<'a> {
    /// Run this agent, in `cwd`.
    Spawn {
        run: &'a str,
        dispatch: &'a str,
        agent_kind: AgentKind,
        subagent_type: &'a str,
        model: &'a str,
        description: String,
        prompt: &'a str,
        cwd: String,
    },
    /// Another turn for an agent that already ran.
    Continue {
        run: &'a str,
        dispatch: &'a str,
        agent: &'a str,
        message: &'a str,
    },
    /// The attempt was cancelled while its agent runs.
    Stop {
        run: &'a str,
        dispatch: &'a str,
        agent: &'a str,
    },
    /// The run ended.
    Done {
        run: &'a str,
        outcome: &'a str,
        receipt: Option<&'a str>,
        summary: Option<Summary>,
        /// The replay trial a `relais dataset replay` recorded for this run.
        #[serde(skip_serializing_if = "Option::is_none")]
        trial: Option<&'a str>,
    },
}

/// What the run's candidate changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
}

impl Summary {
    /// What a unified diff changes: its files, and its added and removed lines.
    pub fn of_patch(patch: &str) -> Self {
        let mut summary = Self {
            files_changed: 0,
            insertions: 0,
            deletions: 0,
        };
        for line in patch.lines() {
            if line.starts_with("diff --git ") {
                summary.files_changed += 1;
            } else if line.starts_with('+') && !line.starts_with("+++") {
                summary.insertions += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                summary.deletions += 1;
            }
        }
        summary
    }

    /// The summary of the run's last candidate, when it kept one: the
    /// highest-numbered `candidate-<n>.patch` in its artifacts directory.
    pub fn of_candidate(artifacts: &Path) -> Option<Self> {
        let latest = std::fs::read_dir(artifacts)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let number = name
                    .strip_prefix("candidate-")?
                    .strip_suffix(".patch")?
                    .parse::<u32>()
                    .ok()?;
                Some((number, entry.path()))
            })
            .max_by_key(|(number, _)| *number)?;
        let patch = std::fs::read_to_string(latest.1).ok()?;
        Some(Self::of_patch(&patch))
    }
}

/// Where protocol lines go: the process's real stdout once [`install`] has
/// kept it, or a sink a test reads. One writer per wire, so lines never
/// interleave.
#[derive(Clone)]
pub struct Wire(Option<Arc<Mutex<Box<dyn Write + Send>>>>);

impl std::fmt::Debug for Wire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Wire(sink)"
        } else {
            "Wire(process)"
        })
    }
}

impl Wire {
    /// The process's own protocol stdout. Lines written before [`install`]
    /// has kept it go nowhere.
    pub fn process() -> Self {
        Self(None)
    }

    /// A wire that writes to `out`, flushing every line.
    pub fn to(out: impl Write + Send + 'static) -> Self {
        Self(Some(Arc::new(Mutex::new(Box::new(out)))))
    }

    /// Write one request line.
    pub fn send(&self, request: &Request<'_>) -> io::Result<()> {
        let line = serde_json::to_string(request).map_err(io::Error::other)?;
        self.write_line(&line)
    }

    fn write_line(&self, line: &str) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        match &self.0 {
            Some(sink) => {
                let mut out = locked(sink);
                out.write_all(&bytes)?;
                out.flush()
            }
            None => match PROTOCOL_STDOUT.get() {
                // One write, under the lock, so lines never interleave.
                Some(stdout) => locked(stdout).write_all(&bytes),
                None => Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "the protocol channel is not installed (run with --protocol)",
                )),
            },
        }
    }
}

/// The real stdout, kept for protocol lines. Set once, by [`install`].
static PROTOCOL_STDOUT: OnceLock<Mutex<File>> = OnceLock::new();

/// The run whose `events.jsonl` receives the process's own stderr.
static STDERR_RUN: Mutex<Option<Arc<EventLog>>> = Mutex::new(None);

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock still holds a usable value: the panic that poisoned
    // it is somebody else's to report, and it is exactly what this module
    // mirrors.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One run's events: the numbering and the file they are appended to.
#[derive(Debug)]
pub struct EventLog {
    run: String,
    path: PathBuf,
    wire: Wire,
    state: Mutex<LogState>,
}

#[derive(Debug)]
struct LogState {
    next_seq: u64,
    next_stderr_seq: u64,
    file: Option<File>,
}

impl EventLog {
    fn emit(&self, event: &Event) {
        let mut state = locked(&self.state);
        let (seq, stderr_seq) = if event.is_protocol_line() {
            state.next_seq += 1;
            (Some(state.next_seq - 1), None)
        } else {
            state.next_stderr_seq += 1;
            (None, Some(state.next_stderr_seq - 1))
        };
        let line = serde_json::to_string(&Line {
            relais: "event",
            run: &self.run,
            seq,
            stderr_seq,
            at: chrono::Utc::now().to_rfc3339(),
            event,
        })
        .expect("an event is plain data");
        if state.file.is_none() {
            state.file = self.open();
        }
        if let Some(file) = state.file.as_mut() {
            // Dropped on failure: events.jsonl is a view of the ledger, and
            // a full disk must not end the run it describes.
            let _ = writeln!(file, "{line}");
        }
        if event.is_protocol_line() {
            // A reader that has gone away loses the line; the run does not
            // end for it, and `events.jsonl` still has it.
            let _ = self.wire.write_line(&line);
        }
    }

    fn open(&self) -> Option<File> {
        // Opened on the first event, when the directory is wanted at all.
        // A failure leaves the file closed and the next event tries again.
        let parent = self.path.parent()?;
        std::fs::create_dir_all(parent).ok()?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok()
    }
}

/// Where a run's events go. Cheap to clone and pass around; a silent one
/// (no run, no file) does nothing.
#[derive(Debug, Clone)]
pub struct Events(Option<Arc<EventLog>>);

impl Events {
    /// Events that go nowhere: a check run outside any run.
    pub fn silent() -> Self {
        Self(None)
    }

    /// The events of `run`, appended under its artifacts directory and
    /// sent to the process's protocol stdout.
    pub fn for_run(run: &str, artifacts: &Path) -> Self {
        Self::for_run_on(run, artifacts, Wire::process())
    }

    /// The events of `run`, sent to `wire`.
    pub fn for_run_on(run: &str, artifacts: &Path, wire: Wire) -> Self {
        Self(Some(Arc::new(EventLog {
            run: run.to_string(),
            path: artifacts.join("events.jsonl"),
            wire,
            state: Mutex::new(LogState {
                next_seq: 0,
                next_stderr_seq: 0,
                file: None,
            }),
        })))
    }

    /// Whether anything is listening, so a caller can skip the work of
    /// following output nobody would see.
    pub fn is_silent(&self) -> bool {
        self.0.is_none()
    }

    pub fn emit(&self, event: Event) {
        if let Some(log) = &self.0 {
            log.emit(&event);
        }
    }

    /// From now on the process's own stderr lines go into this run's
    /// events. Lines that came before were only copied to stderr.
    pub fn mirror_stderr(&self) {
        *locked(&STDERR_RUN) = self.0.clone();
    }
}

/// Every event but `output`, and of `output` only the newest events whose
/// lines fit in [`STATUS_OUTPUT_LINES`], in their original order; then at
/// most [`STATUS_EVENTS`] of them, the newest.
fn bounded_events(events: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut lines_left = STATUS_OUTPUT_LINES;
    let mut keep = vec![true; events.len()];
    for (i, entry) in events.iter().enumerate().rev() {
        if entry["event"]["kind"] != "output" {
            continue;
        }
        let lines = entry["event"]["text"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .count();
        if lines_left == 0 || lines > lines_left {
            lines_left = 0;
            keep[i] = false;
        } else {
            lines_left -= lines;
        }
    }
    let kept: Vec<serde_json::Value> = events
        .into_iter()
        .zip(keep)
        .filter_map(|(entry, keep)| keep.then_some(entry))
        .collect();
    let skip = kept.len().saturating_sub(STATUS_EVENTS);
    kept.into_iter().skip(skip).collect()
}

/// The most `output` lines a status carries.
pub const STATUS_OUTPUT_LINES: usize = 40;

/// The most `events` a status carries: far more than a run's steps,
/// dispatches and decisions, so only a pathological run is cut (its
/// oldest events go).
pub const STATUS_EVENTS: usize = 1000;

/// A run's timeline as `relais native status` prints it: its phases,
/// decisions, cost and outcome, and the last [`STATUS_OUTPUT_LINES`] lines
/// of check output, read from the run's `events.jsonl`. Never the whole file.
///
/// `events` is what the plugin rebuilds its pane from after `/clear` or
/// `/resume`: the event lines themselves, every one but `output`, plus
/// only the newest `output` lines up to [`STATUS_OUTPUT_LINES`] — so the
/// reply stays bounded however much a check printed.
pub fn timeline(run: &str, events_jsonl: &str) -> serde_json::Value {
    let mut events: Vec<serde_json::Value> = Vec::new();
    let mut phases = Vec::new();
    let mut decisions = Vec::new();
    let mut cost = Vec::new();
    let mut outcome = serde_json::Value::Null;
    let mut output: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    for line in events_jsonl.lines() {
        // A line that is not an event (a torn last line of a killed run)
        // says nothing about the timeline.
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let event = &entry["event"];
        if event["kind"].is_string() {
            events.push(entry.clone());
        }
        match event["kind"].as_str() {
            Some("phase") => phases.push(serde_json::json!({
                "at": entry["at"],
                "state": event["state"],
                "reason": event["reason"],
            })),
            Some("decision") => decisions.push(serde_json::json!({
                "at": entry["at"],
                "what": event["what"],
                "reason": event["reason"],
            })),
            Some("cost") => cost.push(serde_json::json!({
                "booked": event["booked"],
                "completeness": event["completeness"],
            })),
            Some("outcome") => {
                outcome = serde_json::json!({
                    "state": event["state"],
                    "receipt": event["receipt"],
                    "summary": event["summary"],
                });
            }
            Some("output") => {
                for text in event["text"].as_str().unwrap_or_default().lines() {
                    if output.len() == STATUS_OUTPUT_LINES {
                        output.pop_front();
                    }
                    output.push_back(text.to_string());
                }
            }
            Some(_) | None => {}
        }
    }
    serde_json::json!({
        "run": run,
        "phases": phases,
        "decisions": decisions,
        "cost": cost,
        "outcome": outcome,
        "output": output,
        "events": bounded_events(events),
    })
}

/// The run whose events were written last, under the artifacts directory.
pub fn latest_run(artifacts: &Path) -> Option<String> {
    std::fs::read_dir(artifacts)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let written = entry
                .path()
                .join("events.jsonl")
                .metadata()
                .ok()?
                .modified()
                .ok()?;
            Some((written, entry.file_name().to_string_lossy().into_owned()))
        })
        .max()
        .map(|(_, run)| run)
}

/// A line the pipe carried, as the text of a `stderr` event.
fn stderr_text(raw: &[u8]) -> String {
    let line = String::from_utf8_lossy(raw);
    let line = line.trim_end_matches(['\n', '\r']);
    let mut end = line.len().min(STDERR_TEXT_BYTES);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_string()
}

/// Copy the pipe to the real stderr, line by line, and into the run's
/// events once there is one.
fn mirror(pipe: File, mut stderr: File) {
    let mut pipe = io::BufReader::new(pipe);
    let mut raw = Vec::new();
    loop {
        raw.clear();
        // A read error ends the mirror: the pipe is the only thing it
        // reads, and there is nowhere to report that it broke.
        match (&mut pipe)
            .take(STDERR_LINE_BYTES)
            .read_until(b'\n', &mut raw)
        {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        // Dropped on failure: stderr itself is what failed.
        let _ = stderr.write_all(&raw);
        let run = locked(&STDERR_RUN).clone();
        if let Some(log) = run {
            log.emit(&Event::Stderr {
                text: stderr_text(&raw),
            });
        }
    }
}

/// The protocol channel while a run is under way; [`Channel::close`] ends it.
#[cfg(unix)]
pub struct Channel {
    stdout: File,
    stderr: File,
    mirror: JoinHandle<()>,
}

#[cfg(not(unix))]
pub struct Channel;

/// Keep the real stdout for protocol lines and send everything else the
/// process writes, to stdout or stderr, to stderr (and, once a run is
/// known, into its events). Call it before anything is printed.
#[cfg(unix)]
pub fn install() -> io::Result<Channel> {
    let diverted = crate::procs::divert_stdio()?;
    let kept_stderr = diverted.stderr.try_clone()?;
    let mirror = std::thread::Builder::new()
        .name("relais-stderr".into())
        .spawn(move || mirror(diverted.reader, kept_stderr))?;
    PROTOCOL_STDOUT
        .set(Mutex::new(diverted.stdout.try_clone()?))
        .map_err(|_| io::Error::other("the protocol channel is already installed"))?;
    Ok(Channel {
        stdout: diverted.stdout,
        stderr: diverted.stderr,
        mirror,
    })
}

#[cfg(not(unix))]
pub fn install() -> io::Result<Channel> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "not supported on Windows yet",
    ))
}

#[cfg(unix)]
impl Channel {
    /// Give the process its stdout and stderr back and let the mirror
    /// finish what the pipe still holds. Waits a bounded time: a grandchild
    /// that kept the pipe open must not hold the exit.
    pub fn close(self) {
        // Dropped on failure: the process is on its way out, and what
        // std still buffers would go to the pipe the mirror is draining.
        let _ = io::stdout().flush();
        let _ = crate::procs::restore_stdio(&self.stdout, &self.stderr);
        let deadline = Instant::now() + DRAIN_WAIT;
        while !self.mirror.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.mirror.is_finished() {
            // The mirror returns nothing and its panic would be news to
            // nobody: the channel is already closed.
            let _ = self.mirror.join();
        }
    }
}

#[cfg(not(unix))]
impl Channel {
    pub fn close(self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .expect("events.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).expect("a json line"))
            .collect()
    }

    #[test]
    fn events_are_numbered_from_zero_in_order_and_appended() {
        let dir = crate::test_support::short_temp_dir("pr-seq");
        let events = Events::for_run("run-1", &dir.join("run-1"));
        events.emit(Event::Decision {
            what: "repair".into(),
            reason: "checks_failed".into(),
        });
        events.emit(Event::Cost {
            booked: Some(5),
            completeness: CostCompleteness::Actual,
        });
        events.emit(Event::Stderr { text: "hi".into() });
        let written = lines(&dir.join("run-1/events.jsonl"));
        let seqs: Vec<Option<u64>> = written.iter().map(|l| l["seq"].as_u64()).collect();
        assert_eq!(seqs, [Some(0), Some(1), None], "a stderr line takes no seq");
        assert_eq!(written[2]["stderr_seq"], 0);
        assert!(written.iter().all(|l| l["relais"] == "event"));
        assert!(written.iter().all(|l| l["run"] == "run-1"));
        assert_eq!(written[0]["event"]["kind"], "decision");
        assert_eq!(written[1]["event"]["completeness"], "actual");
        assert_eq!(written[2]["event"]["text"], "hi");
    }

    #[test]
    fn a_timeline_keeps_phases_decisions_cost_outcome_and_the_last_40_output_lines() {
        let dir = crate::test_support::short_temp_dir("pr-timeline");
        let events = Events::for_run("run-1", &dir.join("run-1"));
        events.emit(Event::Phase {
            state: "running".into(),
            reason: "worker_dispatched".into(),
            detail: serde_json::Value::Null,
        });
        events.emit(Event::Decision {
            what: "repair".into(),
            reason: "checks_failed".into(),
        });
        let first: String = (0..30).map(|n| format!("a{n}\n")).collect();
        let second: String = (0..30).map(|n| format!("b{n}\n")).collect();
        for text in [first, second] {
            events.emit(Event::Output {
                label: "check".into(),
                text,
                elided_bytes: 0,
            });
        }
        events.emit(Event::Cost {
            booked: Some(5),
            completeness: CostCompleteness::Actual,
        });
        events.emit(Event::Outcome {
            state: "accepted".into(),
            receipt: Some("/r/receipt.json".into()),
            summary: Some(Summary {
                files_changed: 1,
                insertions: 3,
                deletions: 1,
            }),
        });
        let text = std::fs::read_to_string(dir.join("run-1/events.jsonl")).expect("events");
        let timeline = timeline("run-1", &text);
        assert_eq!(timeline["run"], "run-1");
        assert_eq!(timeline["phases"][0]["state"], "running");
        assert_eq!(timeline["decisions"][0]["what"], "repair");
        assert_eq!(timeline["cost"][0]["booked"], 5);
        assert_eq!(timeline["outcome"]["state"], "accepted");
        assert_eq!(timeline["outcome"]["summary"]["files_changed"], 1);
        assert_eq!(timeline["outcome"]["summary"]["insertions"], 3);
        assert_eq!(timeline["outcome"]["receipt"], "/r/receipt.json");
        let output = timeline["output"].as_array().expect("output lines");
        assert_eq!(output.len(), STATUS_OUTPUT_LINES);
        assert_eq!(output[0], "a20", "the oldest lines are the ones dropped");
        assert_eq!(output[STATUS_OUTPUT_LINES - 1], "b29");
        // The event lines the plugin rebuilds its pane from: every one but
        // `output`, in order, and only the newest output that fits.
        let kinds: Vec<&str> = timeline["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|entry| entry["event"]["kind"].as_str().expect("a kind"))
            .collect();
        assert_eq!(kinds, ["phase", "decision", "output", "cost", "outcome"]);
        assert!(timeline["events"][2]["event"]["text"]
            .as_str()
            .expect("text")
            .starts_with("b0"));
        assert_eq!(timeline["events"][0]["run"], "run-1");
    }

    #[test]
    fn the_latest_run_is_the_one_whose_events_were_written_last() {
        let dir = crate::test_support::short_temp_dir("pr-latest");
        assert_eq!(latest_run(&dir), None);
        for run in ["run-a", "run-b"] {
            Events::for_run(run, &dir.join(run)).emit(Event::Decision {
                what: "stop".into(),
                reason: "done".into(),
            });
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(latest_run(&dir).as_deref(), Some("run-b"));
    }

    #[test]
    fn a_patch_summary_counts_files_insertions_and_deletions() {
        let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-old\n+new\n+more\n \
                     keep\ndiff --git a/y b/y\ndeleted file mode 100644\n--- a/y\n+++ /dev/null\n-gone\n";
        assert_eq!(
            Summary::of_patch(patch),
            Summary {
                files_changed: 2,
                insertions: 2,
                deletions: 2
            }
        );
    }

    #[test]
    fn requests_and_events_share_one_wire_and_are_one_json_line_each() {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        struct Chunks(std::sync::mpsc::Sender<Vec<u8>>);
        impl Write for Chunks {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.send(buf.to_vec()).map_err(io::Error::other)?;
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let wire = Wire::to(Chunks(tx));
        wire.send(&Request::Stop {
            run: "r",
            dispatch: "d",
            agent: "a",
        })
        .expect("sent");
        wire.send(&Request::Done {
            run: "r",
            outcome: "accepted",
            receipt: None,
            summary: Some(Summary {
                files_changed: 1,
                insertions: 2,
                deletions: 3,
            }),
            trial: None,
        })
        .expect("sent");
        let written: Vec<Vec<u8>> = rx.try_iter().collect();
        assert_eq!(written.len(), 2, "one write per line");
        let stop: serde_json::Value = serde_json::from_slice(&written[0]).expect("json");
        assert_eq!(stop["relais"], "stop");
        assert_eq!(stop["agent"], "a");
        let done: serde_json::Value = serde_json::from_slice(&written[1]).expect("json");
        assert_eq!(done["relais"], "done");
        assert_eq!(done["summary"]["deletions"], 3);
        assert!(done["receipt"].is_null());
        assert!(written.iter().all(|line| line.ends_with(b"\n")));
    }

    #[test]
    fn silent_events_write_nothing() {
        let events = Events::silent();
        events.emit(Event::Stderr { text: "x".into() });
        assert!(events.is_silent());
    }

    #[test]
    fn a_stderr_line_is_trimmed_and_bounded() {
        assert_eq!(stderr_text(b"boom\r\n"), "boom");
        let long = "é".repeat(STDERR_TEXT_BYTES);
        let text = stderr_text(long.as_bytes());
        assert!(text.len() <= STDERR_TEXT_BYTES);
        assert!(text.chars().all(|c| c == 'é'));
    }

    /// The child half of [`the_channel_keeps_stdout_for_protocol_lines_only`]:
    /// it is its own process, since the channel moves the process's own
    /// stdout.
    #[cfg(unix)]
    #[test]
    fn channel_child() {
        let Some(dir) = std::env::var_os("RELAIS_PROTOCOL_TEST_DIR") else {
            return;
        };
        let channel = install().expect("installs");
        let events = Events::for_run("run-child", &PathBuf::from(dir));
        println!("before the run is known");
        events.mirror_stderr();
        events.emit(Event::Decision {
            what: "stop".into(),
            reason: "done".into(),
        });
        println!("a println from inside the run");
        eprintln!("an eprintln from inside the run");
        let panicked = std::panic::catch_unwind(|| panic!("a panic inside the run"));
        assert!(panicked.is_err());
        channel.close();
    }

    #[cfg(unix)]
    #[test]
    fn the_channel_keeps_stdout_for_protocol_lines_only() {
        let dir = crate::test_support::short_temp_dir("pr-chan");
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "protocol::tests::channel_child",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("RELAIS_PROTOCOL_TEST_DIR", dir.as_os_str())
            .output()
            .expect("the child runs");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for line in stdout.lines().filter(|line| line.starts_with('{')) {
            let value: serde_json::Value = serde_json::from_str(line).expect("json");
            assert!(value.get("relais").is_some(), "{line}");
        }
        assert!(stdout.contains(r#""kind":"decision""#), "{stdout}");
        assert!(
            !stdout.contains("a println from inside the run"),
            "{stdout}"
        );
        assert!(
            !stdout.contains("an eprintln from inside the run"),
            "{stdout}"
        );
        assert!(stderr.contains("a println from inside the run"), "{stderr}");
        assert!(
            stderr.contains("an eprintln from inside the run"),
            "{stderr}"
        );
        let written = lines(&dir.join("events.jsonl"));
        let mirrored: Vec<&str> = written
            .iter()
            .filter(|l| l["event"]["kind"] == "stderr")
            .map(|l| l["event"]["text"].as_str().unwrap())
            .collect();
        assert!(mirrored.iter().any(|t| t.contains("a println from inside")));
        assert!(mirrored
            .iter()
            .any(|t| t.contains("an eprintln from inside")));
        assert!(mirrored
            .iter()
            .any(|t| t.contains("a panic inside the run")));
        // The channel's numbering is gapless; mirrored lines have their own.
        let seqs: Vec<u64> = written.iter().filter_map(|l| l["seq"].as_u64()).collect();
        assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>());
        let stderr_seqs: Vec<u64> = written
            .iter()
            .filter(|l| l["event"]["kind"] == "stderr")
            .map(|l| l["stderr_seq"].as_u64().expect("a stderr_seq"))
            .collect();
        assert_eq!(
            stderr_seqs,
            (0..stderr_seqs.len() as u64).collect::<Vec<_>>()
        );
        assert!(written
            .iter()
            .filter(|l| l["event"]["kind"] == "stderr")
            .all(|l| l.get("seq").is_none()));
    }
}
