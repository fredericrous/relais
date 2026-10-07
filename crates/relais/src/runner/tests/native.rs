// -- relais run, native workers (SPEC §23, §29) ------------------------
//
// The engine over a `NativeBackend`, a real `LocalGate`, and a scripted
// relais plugin: a thread that reads the protocol lines the run writes and
// calls the gate the way `relais native` does.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::mpsc;
use std::sync::Arc;

use super::*;
use crate::adapter::native::{Link, NativeBackend, Waits};
use crate::admission::{
    AgentStatus, AgentUsage, BoundOutcome, LocalGate, StoppedOutcome, StoppedReport,
};
use crate::orchestration::{ModelPrice, PriceTable};
use crate::protocol::Wire;

const SESSION: &str = "test-session";

/// Every flush is one protocol line, sent to the scripted plugin.
struct LineSink {
    lines: mpsc::Sender<String>,
    pending: Vec<u8>,
}

impl LineSink {
    fn wire() -> (Wire, mpsc::Receiver<String>) {
        let (lines, rx) = mpsc::channel();
        let sink = Self {
            lines,
            pending: Vec::new(),
        };
        (Wire::to(sink), rx)
    }
}

impl Write for LineSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let line = String::from_utf8_lossy(&self.pending)
            .trim_end()
            .to_string();
        self.pending.clear();
        self.lines.send(line).map_err(std::io::Error::other)
    }
}

/// What the plugin was asked, and did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Spawn {
        dispatch_id: String,
        agent_id: String,
        agent_kind: String,
        subagent_type: String,
        model: String,
        cwd: PathBuf,
    },
    Continue {
        dispatch_id: String,
        agent_id: String,
    },
    Stop {
        dispatch_id: String,
        agent_id: String,
    },
}

/// How the scripted plugin treats a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plugin {
    /// Spawns or continues as asked, works, binds and reports the stop.
    Obeys,
    /// The agent's run ends `failed`.
    AgentFails,
    /// Spawns and binds, and the agent never stops.
    NeverStops,
    /// Binds, then cancels the run.
    CancelsAfterBound,
    /// Reports the stop before the bind.
    StopsBeforeBound,
    /// Reports the stop twice, the second time with other numbers.
    StopsTwice,
    /// The agent leaves no transcript behind.
    LeavesNoTranscript,
}

/// The agent's work on its tree, given the prompt it was sent: the task
/// fixture's change worker, fixing on the first attempt only when the
/// prompt says so.
type Work = fn(&Path, &str);

fn fixes_at_once(tree: &Path, _prompt: &str) {
    std::fs::remove_file(tree.join("src/main.rs")).expect("fix");
}

/// Nothing at first, something on a repair, the fix on an escalation.
fn fixes_on_escalation(tree: &Path, prompt: &str) {
    if prompt.contains("escalation addendum") {
        std::fs::remove_file(tree.join("src/main.rs")).expect("remove");
    } else if prompt.contains("repair addendum") {
        std::fs::write(tree.join("src/notes.txt"), "investigation\n").expect("write");
    }
}

struct Agent {
    kind: String,
    tree: PathBuf,
    model: String,
    messages: u32,
}

/// The messages an agent's turn adds to its transcript. Each message is
/// written twice, its output growing, as Claude Code does.
const SPAWN_MESSAGES: u32 = 2;
const CONTINUE_MESSAGES: u32 = 1;

fn append_messages(file: &Path, agent_id: &str, agent: &mut Agent, count: u32) {
    std::fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .expect("transcript opens");
    for _ in 0..count {
        agent.messages += 1;
        for output in [5, 9] {
            writeln!(
                out,
                r#"{{"type":"assistant","timestamp":"2026-10-06T00:00:00Z","message":{{"id":"{agent_id}-m{}","model":"{}","usage":{{"input_tokens":10,"output_tokens":{output},"cache_read_input_tokens":0}}}}}}"#,
                agent.messages, agent.model
            )
            .expect("transcript line");
        }
    }
}

/// The concrete id Claude Code reports for an alias.
fn concrete(model: &str) -> String {
    format!("claude-{model}-5")
}

/// What the scripted plugin saw and answered.
#[derive(Default)]
struct Scripted {
    seen: Vec<Seen>,
    /// Every line the run wrote, parsed.
    lines: Vec<serde_json::Value>,
    bound: Vec<BoundOutcome>,
    stopped: Vec<StoppedOutcome>,
}

fn record_bound(script: &mut Scripted, gate: &LocalGate, dispatch: &str, agent: &str) {
    script
        .bound
        .push(gate.native_bound(dispatch, agent).expect("bound"));
}

fn record_stop(script: &mut Scripted, gate: &LocalGate, dispatch: &str, report: &StoppedReport) {
    script
        .stopped
        .push(gate.native_stopped(dispatch, report).expect("stopped"));
}

fn run_plugin(
    gate: Arc<LocalGate>,
    lines: mpsc::Receiver<String>,
    projects: PathBuf,
    plugin: Plugin,
    work: Work,
) -> Scripted {
    let mut script = Scripted::default();
    let mut agents: BTreeMap<String, Agent> = BTreeMap::new();
    let mut spawned = 0;
    // Until the wire closes; once `done` has been read, only a short grace
    // for anything written after it (which the stream tests would catch).
    let mut patience = Duration::from_secs(30);
    while let Ok(line) = lines.recv_timeout(patience) {
        let value: serde_json::Value =
            serde_json::from_str(&line).expect("every line on the wire is json");
        script.lines.push(value.clone());
        let text = |key: &str| value[key].as_str().map(str::to_string);
        let word = text("relais").expect("every line is a protocol object");
        let (dispatch_id, agent_id, prompt, fresh) = match word.as_str() {
            "event" => continue,
            "done" => {
                patience = Duration::from_millis(300);
                continue;
            }
            "stop" => {
                script.seen.push(Seen::Stop {
                    dispatch_id: text("dispatch").expect("dispatch"),
                    agent_id: text("agent").expect("agent"),
                });
                continue;
            }
            "spawn" => {
                spawned += 1;
                let agent_id = format!("ag{spawned}");
                let cwd = PathBuf::from(text("cwd").expect("cwd"));
                assert!(cwd.is_dir(), "the spawn names the attempt's worktree");
                assert!(!text("prompt").expect("prompt").contains("relais-dispatch"));
                let model = text("model").expect("model");
                let kind = text("agent_kind").expect("agent_kind");
                script.seen.push(Seen::Spawn {
                    dispatch_id: text("dispatch").expect("dispatch"),
                    agent_id: agent_id.clone(),
                    agent_kind: kind.clone(),
                    subagent_type: text("subagent_type").expect("subagent_type"),
                    model: model.clone(),
                    cwd: cwd.clone(),
                });
                agents.insert(
                    agent_id.clone(),
                    Agent {
                        kind,
                        tree: cwd,
                        model: concrete(&model),
                        messages: 0,
                    },
                );
                (
                    text("dispatch").expect("dispatch"),
                    agent_id,
                    text("prompt").expect("prompt"),
                    true,
                )
            }
            "continue" => {
                let agent_id = text("agent").expect("agent");
                script.seen.push(Seen::Continue {
                    dispatch_id: text("dispatch").expect("dispatch"),
                    agent_id: agent_id.clone(),
                });
                (
                    text("dispatch").expect("dispatch"),
                    agent_id,
                    text("message").expect("message"),
                    false,
                )
            }
            other => panic!("an unknown request {other}"),
        };
        let agent = agents.get_mut(&agent_id).expect("a known agent");
        // Only a worker changes its tree; a reviewer and a planner read.
        if agent.kind == "worker" {
            work(&agent.tree, &prompt);
        }
        let count = if fresh {
            SPAWN_MESSAGES
        } else {
            CONTINUE_MESSAGES
        };
        if plugin == Plugin::LeavesNoTranscript {
            agent.messages += count;
        } else {
            let transcript = projects
                .join("-slug")
                .join(SESSION)
                .join("subagents")
                .join(format!("agent-{agent_id}.jsonl"));
            append_messages(&transcript, &agent_id, agent, count);
        }
        let report = |factor: u64, status: AgentStatus| StoppedReport {
            agent_id: agent_id.clone(),
            status,
            usage: Some(AgentUsage {
                input_tokens: Some(10 * u64::from(count) * factor),
                output_tokens: Some(9 * u64::from(count) * factor),
                cache_read_input_tokens: Some(0),
                cache_creation_input_tokens: Some(0),
                model: Some(agent.model.clone()),
            }),
            answer: Some(answer_of(&agent.kind).into()),
        };
        match plugin {
            Plugin::StopsBeforeBound => {
                let stopped = report(1, AgentStatus::Completed);
                record_stop(&mut script, &gate, &dispatch_id, &stopped);
                record_bound(&mut script, &gate, &dispatch_id, &agent_id);
            }
            Plugin::NeverStops | Plugin::CancelsAfterBound => {
                record_bound(&mut script, &gate, &dispatch_id, &agent_id);
                if plugin == Plugin::CancelsAfterBound {
                    // Long enough for the backend's poll to see the bind.
                    std::thread::sleep(Duration::from_millis(700));
                    let runs: Vec<String> = gate.status().runs.keys().cloned().collect();
                    for run in runs {
                        gate.cancel_run(&run);
                    }
                }
            }
            Plugin::Obeys
            | Plugin::AgentFails
            | Plugin::StopsTwice
            | Plugin::LeavesNoTranscript => {
                record_bound(&mut script, &gate, &dispatch_id, &agent_id);
                let status = if plugin == Plugin::AgentFails {
                    AgentStatus::Failed
                } else {
                    AgentStatus::Completed
                };
                record_stop(&mut script, &gate, &dispatch_id, &report(1, status));
                if plugin == Plugin::StopsTwice {
                    record_stop(&mut script, &gate, &dispatch_id, &report(2, status));
                }
            }
        }
    }
    script
}

/// What an agent of this kind ends its turn with.
fn answer_of(kind: &str) -> &'static str {
    match kind {
        "reviewer" => "review ok\nFINDINGS: none",
        "planner" => r#"{"packages":[],"integration_acceptance":[]}"#,
        _ => "DONE",
    }
}

fn prices() -> PriceTable {
    // One micro-dollar per input token and two per output token.
    PriceTable {
        version: "test".into(),
        models: ["sonnet", "fable"]
            .iter()
            .map(|model| ModelPrice {
                ids: vec![concrete(model)],
                input: 1_000_000,
                output: 2_000_000,
                cache_read: 0,
                cache_write_5m: 0,
                cache_write_1h: 0,
                fast_input: None,
                fast_output: None,
            })
            .collect(),
    }
}

/// The scripted plugin: how it treats requests, and the agent's work.
#[derive(Clone, Copy)]
struct Cast {
    plugin: Plugin,
    work: Work,
}

/// What a native run left behind.
struct Native {
    outcome: RunOutcome,
    script: Scripted,
}

impl Native {
    fn seen(&self) -> &[Seen] {
        &self.script.seen
    }
}

fn waits(hello_lapse: Duration) -> Waits {
    Waits {
        spawn: Duration::from_secs(5),
        hello_lapse,
    }
}

fn run_native(
    fixture: &Fixture,
    review: Review,
    attempts: u32,
    plugin: Plugin,
    work: Work,
    prices: Option<PriceTable>,
    configure: impl FnOnce(&mut RepoPolicy),
) -> Native {
    let cast = Cast { plugin, work };
    run_native_waiting(
        fixture,
        review,
        attempts,
        cast,
        prices,
        waits(Duration::from_secs(60)),
        configure,
    )
}

fn run_native_waiting(
    fixture: &Fixture,
    review: Review,
    attempts: u32,
    cast: Cast,
    prices: Option<PriceTable>,
    waits: Waits,
    configure: impl FnOnce(&mut RepoPolicy),
) -> Native {
    run_native_contract(
        fixture,
        &fixture.contract(review),
        attempts,
        cast,
        prices,
        waits,
        configure,
    )
}

fn run_native_contract(
    fixture: &Fixture,
    contract: &TaskContract,
    attempts: u32,
    cast: Cast,
    prices: Option<PriceTable>,
    waits: Waits,
    configure: impl FnOnce(&mut RepoPolicy),
) -> Native {
    let mut repo = fixture.repo_policy(vec![main_gone_check()], attempts);
    configure(&mut repo);
    let machine = fixture.machine_for(&repo);
    let gate = Arc::new(LocalGate::new(ConcurrencyLimits::default()));
    gate.native_hello(SESSION).expect("hello");
    let harness = MockBackend::new(|_| MockOutcome::default());
    let (wire, rx) = LineSink::wire();
    let projects = fixture.dir.join("projects");
    let backend = NativeBackend::new(
        &harness,
        gate.as_ref(),
        Link {
            run_id: "run-under-test".into(),
            session_id: SESSION.to_string(),
            wire: wire.clone(),
            projects_dir: Some(projects.clone()),
        },
        waits,
        prices,
    );
    let parent_gate = Arc::clone(&gate);
    let session =
        std::thread::spawn(move || run_plugin(parent_gate, rx, projects, cast.plugin, cast.work));
    let outcome = fixture.execute_presented(
        contract,
        &repo,
        &machine,
        &backend,
        gate.as_ref(),
        Presented { wire: wire.clone() },
    );
    // Every holder of the wire gone ends the plugin's loop.
    drop(backend);
    drop(wire);
    let script = session.join().expect("the scripted plugin");
    Native { outcome, script }
}

fn ledger_of(fixture: &Fixture) -> rusqlite::Connection {
    rusqlite::Connection::open(fixture.dir.join("ledger.sqlite")).expect("the ledger opens")
}

/// The run's worker dispatches, oldest first: source and agent id.
fn dispatch_rows(fixture: &Fixture, run_id: &RunId) -> Vec<(String, Option<String>)> {
    let conn = ledger_of(fixture);
    let mut stmt = conn
        .prepare(
            "SELECT source, agent_id FROM dispatches
             WHERE run_id = ?1 AND attempt_id IS NOT NULL ORDER BY created_at, rowid",
        )
        .expect("prepare");
    stmt.query_map([run_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect()
}

/// A native run's usage rows: input, output, cost, kind and completeness.
type UsageRow = (Option<i64>, Option<i64>, Option<i64>, String, String);

fn native_usage(fixture: &Fixture, run_id: &RunId) -> Vec<UsageRow> {
    let conn = ledger_of(fixture);
    let mut stmt = conn
        .prepare(
            "SELECT input_tokens, output_tokens, cost_micros, cost_kind, completeness
             FROM usage_events
             WHERE run_id = ?1 AND attempt_id IS NOT NULL AND phase != 'review'
             ORDER BY rowid",
        )
        .expect("prepare");
    stmt.query_map([run_id.as_str()], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })
    .expect("query")
    .map(|row| row.expect("row"))
    .collect()
}

/// The message ids the run recorded as booked.
fn booked_ids(fixture: &Fixture) -> Vec<String> {
    let conn = ledger_of(fixture);
    let mut stmt = conn
        .prepare("SELECT message_id FROM native_usage_messages ORDER BY message_id")
        .expect("prepare");
    stmt.query_map([], |row| row.get(0))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect()
}

fn interrupted_detail(outcome: &RunOutcome) -> &str {
    let Terminal::Interrupted { detail } = &outcome.terminal else {
        panic!("expected an interrupted run, got {outcome:?}");
    };
    detail
}

/// Every line a run wrote is a protocol object, and the last one is `done`.
fn assert_protocol_stream(run: &Native) -> &serde_json::Value {
    assert!(!run.script.lines.is_empty());
    for line in &run.script.lines {
        assert!(line["relais"].is_string(), "not a protocol object: {line}");
    }
    let last = run.script.lines.last().expect("a last line");
    assert_eq!(last["relais"], "done", "the last line ends the run");
    assert_eq!(last["run"], run.outcome.run_id.as_str());
    assert_eq!(last["outcome"], run.outcome.state().as_str());
    assert_eq!(
        run.script
            .lines
            .iter()
            .filter(|line| line["relais"] == "done")
            .count(),
        1,
        "one done per run"
    );
    last
}

#[test]
fn a_change_task_is_accepted_after_a_worker_spawn_and_a_reviewer_spawn() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Required,
        3,
        Plugin::Obeys,
        fixes_at_once,
        Some(prices()),
        |_| {},
    );
    assert!(
        matches!(run.outcome.terminal, Terminal::Accepted(_)),
        "expected acceptance, got {:?}",
        run.outcome
    );
    let spawns: Vec<_> = run
        .seen()
        .iter()
        .filter_map(|seen| match seen {
            Seen::Spawn {
                agent_id,
                agent_kind,
                subagent_type,
                model,
                cwd,
                ..
            } => Some((agent_id, agent_kind, subagent_type, model, cwd)),
            Seen::Continue { .. } | Seen::Stop { .. } => None,
        })
        .collect();
    let [(agent_id, worker, worker_type, model, worker_cwd), (reviewer_id, reviewer, reviewer_type, reviewer_model, reviewer_cwd)] =
        spawns.as_slice()
    else {
        panic!("a worker and a reviewer expected, saw {:?}", run.seen());
    };
    assert_eq!(worker.as_str(), "worker");
    assert_eq!(worker_type.as_str(), "relais:relais-worker-sonnet-default");
    assert_eq!(model.as_str(), "sonnet");
    assert_eq!(reviewer.as_str(), "reviewer");
    assert_eq!(
        reviewer_type.as_str(),
        format!("relais:relais-reviewer-{reviewer_model}-default")
    );
    // The reviewer reads the review directory: the task worktree the
    // worker left its candidate in.
    assert_eq!(reviewer_cwd, worker_cwd);
    // The worker's dispatch row is native and names its agent.
    assert_eq!(
        dispatch_rows(&fixture, &run.outcome.run_id),
        vec![("native_run".to_string(), Some((*agent_id).clone()))]
    );
    // The reviewer's row is native too and names its agent, and its cost
    // is the price table's estimate, like the worker's.
    let conn = ledger_of(&fixture);
    let review_row: (String, Option<String>) = conn
        .query_row(
            "SELECT source, agent_id FROM dispatches WHERE run_id = ?1 AND attempt_id IS NULL",
            [run.outcome.run_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("one reviewer row");
    assert_eq!(
        review_row,
        ("native_run".to_string(), Some((*reviewer_id).clone()))
    );
    let review_kind: String = conn
        .query_row(
            "SELECT cost_kind FROM usage_events WHERE run_id = ?1 AND phase = 'review'",
            [run.outcome.run_id.as_str()],
            |row| row.get(0),
        )
        .expect("the review's usage row");
    assert!(
        review_kind.contains("EstimatedApiEquivalent"),
        "{review_kind}"
    );
    // One bind and one stop for each of the two agents.
    assert_eq!(run.script.bound, [BoundOutcome::Bound, BoundOutcome::Bound]);
    assert_eq!(
        run.script.stopped,
        [StoppedOutcome::Recorded, StoppedOutcome::Recorded]
    );
    let done = assert_protocol_stream(&run);
    let receipt = done["receipt"].as_str().expect("a receipt path");
    assert!(receipt.ends_with("receipt.json"), "{receipt}");
    assert_eq!(done["summary"]["files_changed"], 1, "{done}");
    assert_eq!(done["summary"]["insertions"], 0, "{done}");
    assert!(
        done["summary"]["deletions"].as_u64().unwrap_or(0) > 0,
        "{done}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_decomposed_task_spawns_its_planner_in_the_repository_before_its_worker() {
    let fixture = Fixture::new();
    let contract = TaskContract::from_json_str(
        &serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "objective": "Remove the obsolete entry point",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "acceptance": ["src/main.rs no longer exists"],
            "verification_profile": "profile",
            "decomposition": "propose",
        })
        .to_string(),
    )
    .expect("contract");
    let run = run_native_contract(
        &fixture,
        &contract,
        3,
        Cast {
            plugin: Plugin::Obeys,
            work: fixes_at_once,
        },
        Some(prices()),
        waits(Duration::from_secs(60)),
        |_| {},
    );
    assert!(
        matches!(run.outcome.terminal, Terminal::Accepted(_)),
        "expected acceptance, got {:?}",
        run.outcome
    );
    let spawns: Vec<_> = run
        .seen()
        .iter()
        .filter_map(|seen| match seen {
            Seen::Spawn {
                agent_kind,
                subagent_type,
                cwd,
                ..
            } => Some((agent_kind.as_str(), subagent_type.as_str(), cwd)),
            Seen::Continue { .. } | Seen::Stop { .. } => None,
        })
        .collect();
    let [("planner", planner_type, planner_cwd), ("worker", worker_type, _)] = spawns.as_slice()
    else {
        panic!("a planner then a worker expected, saw {:?}", run.seen());
    };
    assert_eq!(*planner_type, "relais:relais-planner-haiku-default");
    assert_eq!(*worker_type, "relais:relais-worker-sonnet-default");
    assert_eq!(*planner_cwd, &fixture.repo);
    // Every dispatch row of the run, the planner's included, is native
    // and names its agent; the planning usage is the price table's estimate.
    let conn = ledger_of(&fixture);
    let managed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dispatches WHERE source != 'native_run' OR agent_id IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(managed, 0, "every dispatch row is native with its agent");
    let planning_kind: String = conn
        .query_row(
            "SELECT cost_kind FROM usage_events WHERE phase = 'planning'",
            [],
            |row| row.get(0),
        )
        .expect("the planning usage row");
    assert!(
        planning_kind.contains("EstimatedApiEquivalent"),
        "{planning_kind}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_repair_continues_the_same_agent_and_an_escalation_spawns_on_the_next_model() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::Obeys,
        fixes_on_escalation,
        Some(prices()),
        |_| {},
    );
    let RunOutcome {
        run_id,
        terminal: Terminal::Accepted(receipt),
    } = &run.outcome
    else {
        panic!("expected acceptance, got {:?}", run.outcome);
    };
    assert_eq!(receipt.attempts, 3);
    assert_eq!(
        receipt.models_used,
        vec!["claude-sonnet-5", "claude-fable-5"],
        "the models the agents reported"
    );
    let kinds: Vec<_> = run
        .seen()
        .iter()
        .map(|seen| match seen {
            Seen::Spawn {
                subagent_type,
                agent_id,
                ..
            } => format!("spawn {subagent_type} {agent_id}"),
            Seen::Continue { agent_id, .. } => format!("continue {agent_id}"),
            Seen::Stop { agent_id, .. } => format!("stop {agent_id}"),
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "spawn relais:relais-worker-sonnet-default ag1",
            "continue ag1",
            "spawn relais:relais-worker-fable-default ag2",
        ]
    );
    assert_eq!(
        dispatch_rows(&fixture, run_id),
        vec![
            ("native_run".to_string(), Some("ag1".to_string())),
            ("native_run".to_string(), Some("ag1".to_string())),
            ("native_run".to_string(), Some("ag2".to_string())),
        ]
    );
    assert_protocol_stream(&run);
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn usage_is_booked_from_the_reported_numbers_and_the_transcript_ids_are_recorded() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::Obeys,
        fixes_on_escalation,
        Some(prices()),
        |_| {},
    );
    assert!(matches!(run.outcome.terminal, Terminal::Accepted(_)));
    let rows = native_usage(&fixture, &run.outcome.run_id);
    // Per dispatch: the spawn reports 2 messages' worth, the continuation 1,
    // the escalation's fresh agent 2.
    let tokens: Vec<_> = rows.iter().map(|row| (row.0, row.1)).collect();
    assert_eq!(
        tokens,
        vec![
            (Some(20), Some(18)),
            (Some(10), Some(9)),
            (Some(20), Some(18))
        ],
        "each attempt books what its dispatch reported: {rows:?}"
    );
    let costs: Vec<_> = rows.iter().map(|row| row.2).collect();
    assert_eq!(costs, vec![Some(56), Some(28), Some(56)]);
    for row in &rows {
        assert!(
            row.3.contains("EstimatedApiEquivalent"),
            "booked as an estimate: {row:?}"
        );
        assert!(row.4.contains("estimated"), "{row:?}");
    }
    // The transcripts are read for their ids only, for a revert's sake.
    assert_eq!(
        booked_ids(&fixture),
        ["ag1-m1", "ag1-m2", "ag1-m3", "ag2-m1", "ag2-m2"]
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_missing_transcript_books_the_usage_and_says_rollback_ids_missing() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::LeavesNoTranscript,
        fixes_at_once,
        Some(prices()),
        |_| {},
    );
    assert!(matches!(run.outcome.terminal, Terminal::Accepted(_)));
    let rows = native_usage(&fixture, &run.outcome.run_id);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].0, rows[0].1, rows[0].2),
        (Some(20), Some(18), Some(56))
    );
    assert!(booked_ids(&fixture).is_empty());
    let events = std::fs::read_to_string(
        fixture
            .artifacts
            .join(run.outcome.run_id.as_str())
            .join("events.jsonl"),
    )
    .expect("events.jsonl");
    let missing: Vec<serde_json::Value> = events
        .lines()
        .map(|line| serde_json::from_str(line).expect("json"))
        .filter(|line: &serde_json::Value| line["event"]["what"] == "rollback_ids_missing")
        .collect();
    assert_eq!(missing.len(), 1, "{events}");
    assert_eq!(missing[0]["event"]["kind"], "decision");
    let [Seen::Spawn { dispatch_id, .. }] = run.seen() else {
        panic!("one spawn expected");
    };
    assert_eq!(missing[0]["event"]["reason"], dispatch_id.as_str());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_stopped_reported_twice_books_once() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::StopsTwice,
        fixes_at_once,
        Some(prices()),
        |_| {},
    );
    assert!(matches!(run.outcome.terminal, Terminal::Accepted(_)));
    assert_eq!(
        run.script.stopped,
        [StoppedOutcome::Recorded, StoppedOutcome::AlreadyStopped]
    );
    let rows = native_usage(&fixture, &run.outcome.run_id);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].0, rows[0].1), (Some(20), Some(18)), "{rows:?}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_stopped_reported_before_the_bind_is_kept() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::StopsBeforeBound,
        fixes_at_once,
        Some(prices()),
        |_| {},
    );
    assert!(
        matches!(run.outcome.terminal, Terminal::Accepted(_)),
        "{:?}",
        run.outcome
    );
    assert_eq!(run.script.stopped, [StoppedOutcome::Recorded]);
    assert_eq!(run.script.bound, [BoundOutcome::Bound]);
    let rows = native_usage(&fixture, &run.outcome.run_id);
    assert_eq!((rows[0].0, rows[0].1), (Some(20), Some(18)), "{rows:?}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn without_a_price_table_the_cost_is_unknown_never_zero() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::Obeys,
        fixes_at_once,
        None,
        |_| {},
    );
    assert!(matches!(run.outcome.terminal, Terminal::Accepted(_)));
    let rows = native_usage(&fixture, &run.outcome.run_id);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, Some(20), "the tokens are still known");
    assert_eq!(rows[0].2, None, "an unpriced cost is not zero: {rows:?}");
    assert!(rows[0].4.contains("unknown"), "{rows:?}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// The test prices without the sonnet id: the model attempt 1 runs as.
fn prices_without_sonnet() -> PriceTable {
    let mut table = prices();
    table
        .models
        .retain(|price| price.ids != [concrete("sonnet")]);
    table
}

#[test]
fn an_unpriced_model_that_fails_verification_ends_blocked_with_no_second_dispatch() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::Obeys,
        fixes_on_escalation,
        Some(prices_without_sonnet()),
        |_| {},
    );
    let Terminal::Blocked { code, detail } = &run.outcome.terminal else {
        panic!("expected a blocked run, got {:?}", run.outcome);
    };
    assert_eq!(*code, BlockCode::NativeUnpriced);
    assert!(detail.contains("native_unpriced"), "{detail}");
    assert!(detail.contains("claude-sonnet-5"), "{detail}");
    assert!(detail.contains("[pricing.models]"), "{detail}");
    assert!(
        matches!(run.seen(), [Seen::Spawn { .. }]),
        "no request line after the first: {:?}",
        run.seen()
    );
    assert_eq!(
        dispatch_rows(&fixture, &run.outcome.run_id).len(),
        1,
        "no second admission request"
    );
    // The attempt itself was booked, its cost unknown.
    let rows = native_usage(&fixture, &run.outcome.run_id);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2, None, "{rows:?}");
    assert!(
        rows[0].3.contains("EstimatedApiEquivalent"),
        "an unpriced native cost is still an estimate: {rows:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_priced_model_that_fails_verification_repairs_as_before() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::Obeys,
        fixes_on_escalation,
        Some(prices()),
        |_| {},
    );
    assert!(
        matches!(run.outcome.terminal, Terminal::Accepted(_)),
        "{:?}",
        run.outcome
    );
    assert!(
        run.seen()
            .iter()
            .any(|seen| matches!(seen, Seen::Continue { .. })),
        "the repair continued the agent: {:?}",
        run.seen()
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn an_attempt_accepted_with_an_unpriced_record_stays_accepted() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::Obeys,
        fixes_at_once,
        Some(prices_without_sonnet()),
        |_| {},
    );
    assert!(
        matches!(run.outcome.terminal, Terminal::Accepted(_)),
        "{:?}",
        run.outcome
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn an_attempt_with_no_booked_record_has_no_unpriced_model() {
    let result = LaunchResult {
        dispatch_id: "d".into(),
        ended: Ended::Exited(0),
        stdout: String::new(),
        stderr: String::new(),
        result_text: Some("done".into()),
        session_id: None,
        effective_model: Some("claude-sonnet-5".into()),
        usage: crate::backend::UsageReport::unknown(),
        worker_claims_blockage: false,
        failure_detail: None,
        booked_message_ids: Vec::new(),
        unpriced: Vec::new(),
    };
    assert_eq!(
        unpriced_native_model(&result),
        None,
        "no booked record, nothing to price"
    );
    let booked = LaunchResult {
        booked_message_ids: vec!["m1".into()],
        unpriced: vec!["claude-sonnet-5 has no [pricing.models] entry".into()],
        ..result
    };
    assert_eq!(
        unpriced_native_model(&booked),
        Some(vec![
            "claude-sonnet-5 has no [pricing.models] entry".to_string()
        ])
    );
}

/// The protocol requests among the lines a run wrote.
fn requests(lines: &mpsc::Receiver<String>) -> Vec<serde_json::Value> {
    lines
        .try_iter()
        .map(|line| serde_json::from_str(&line).expect("json"))
        .filter(|line: &serde_json::Value| {
            ["spawn", "continue", "stop"].contains(&line["relais"].as_str().unwrap_or_default())
        })
        .collect()
}

#[test]
fn no_bind_within_the_wait_ends_the_attempt_interrupted_and_the_run_stops() {
    let fixture = Fixture::new();
    let mut repo = fixture.repo_policy(vec![main_gone_check()], 3);
    repo.execution.max_wall_seconds = 60;
    let machine = fixture.machine_for(&repo);
    let gate = LocalGate::new(ConcurrencyLimits::default());
    gate.native_hello(SESSION).expect("hello");
    let harness = MockBackend::new(|_| MockOutcome::default());
    let (wire, rx) = LineSink::wire();
    let backend = NativeBackend::new(
        &harness,
        &gate,
        Link {
            run_id: "run-under-test".into(),
            session_id: SESSION.to_string(),
            wire,
            projects_dir: None,
        },
        Waits {
            spawn: Duration::from_millis(600),
            hello_lapse: Duration::from_secs(60),
        },
        Some(prices()),
    );
    let outcome = fixture.execute_presented(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &backend,
        &gate,
        Presented {
            wire: Wire::process(),
        },
    );
    let detail = interrupted_detail(&outcome);
    assert!(detail.contains("native_spawn_missing"), "{detail}");
    assert!(detail.contains("no session spawned it"), "{detail}");
    assert_eq!(
        requests(&rx).len(),
        1,
        "the request was asked once, and the run did not go on to a second attempt"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_model_without_a_shipped_definition_ends_interrupted_before_any_spawn_line() {
    let fixture = Fixture::new();
    let mut repo = fixture.repo_policy(vec![main_gone_check()], 3);
    repo.models
        .get_mut(&Tier::Implementation)
        .expect("an implementation tier")
        .id = "claude-sonnet-5-5".into();
    let machine = fixture.machine_for(&repo);
    let gate = LocalGate::new(ConcurrencyLimits::default());
    gate.native_hello(SESSION).expect("hello");
    let harness = MockBackend::new(|_| MockOutcome::default());
    let (wire, rx) = LineSink::wire();
    let backend = NativeBackend::new(
        &harness,
        &gate,
        Link {
            run_id: "run-under-test".into(),
            session_id: SESSION.to_string(),
            wire,
            projects_dir: None,
        },
        waits(Duration::from_secs(60)),
        Some(prices()),
    );
    let outcome = fixture.execute_presented(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &backend,
        &gate,
        Presented {
            wire: Wire::process(),
        },
    );
    let detail = interrupted_detail(&outcome);
    assert!(detail.contains("native_worker_missing"), "{detail}");
    assert!(detail.contains("claude-sonnet-5-5"), "{detail}");
    assert_eq!(requests(&rx).len(), 0, "nothing was written");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// The request line cannot be written: the launch fails before any
/// agent exists, and the dispatch row still says what the
/// coordinator was told, `native_run`, with no agent.
#[test]
fn a_native_launch_that_fails_keeps_its_row_native() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("the plugin's pipe is gone"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("the plugin's pipe is gone"))
        }
    }
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let machine = fixture.machine_for(&repo);
    let gate = LocalGate::new(ConcurrencyLimits::default());
    gate.native_hello(SESSION).expect("hello");
    let harness = MockBackend::new(|_| MockOutcome::default());
    let backend = NativeBackend::new(
        &harness,
        &gate,
        Link {
            run_id: "run-under-test".into(),
            session_id: SESSION.to_string(),
            wire: Wire::to(Broken),
            projects_dir: None,
        },
        waits(Duration::from_secs(60)),
        Some(prices()),
    );
    let outcome = fixture.execute_presented(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &backend,
        &gate,
        Presented {
            wire: Wire::process(),
        },
    );
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "{:?}",
        outcome.terminal
    );
    assert_eq!(
        dispatch_rows(&fixture, &outcome.run_id),
        vec![("native_run".to_string(), None)]
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn an_agent_reported_failed_ends_the_attempt_interrupted_and_the_stream_still_ends_done() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::AgentFails,
        fixes_at_once,
        Some(prices()),
        |_| {},
    );
    let detail = interrupted_detail(&run.outcome);
    assert!(detail.contains("failed"), "{detail}");
    let done = assert_protocol_stream(&run);
    assert_eq!(done["outcome"], "interrupted");
    // The failed agent's spend is still booked.
    let rows = native_usage(&fixture, &run.outcome.run_id);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, Some(20), "{rows:?}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn an_agent_that_never_stops_ends_at_the_wall_timeout() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::NeverStops,
        fixes_at_once,
        Some(prices()),
        // The attempt's wall time is what is left of the run's: one second.
        |repo| repo.execution.max_wall_seconds = 1,
    );
    let detail = interrupted_detail(&run.outcome);
    assert!(detail.contains("wall time"), "{detail}");
    let transitions = fixture
        .ledger
        .transitions(&run.outcome.run_id)
        .expect("history");
    assert!(
        transitions.iter().any(|transition| transition
            .detail
            .as_ref()
            .is_some_and(|detail| detail["timed_out"] == true)),
        "the wait was recorded as a timeout: {transitions:?}"
    );
    // The agent is told to stop: left running, it would go on editing the
    // attempt's worktree and spending what nothing books.
    assert!(
        run.seen()
            .iter()
            .any(|seen| matches!(seen, Seen::Stop { .. })),
        "a stop line on timeout: {:?}",
        run.seen()
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_session_whose_hello_lapses_ends_the_attempt_mod_gone() {
    let fixture = Fixture::new();
    let cast = Cast {
        plugin: Plugin::NeverStops,
        work: fixes_at_once,
    };
    let run = run_native_waiting(
        &fixture,
        Review::Optional,
        3,
        cast,
        Some(prices()),
        // The short clock: the plugin said hello once, at the start.
        waits(Duration::from_millis(400)),
        |_| {},
    );
    let detail = interrupted_detail(&run.outcome);
    assert!(detail.contains("mod_gone"), "{detail}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_run_cancelled_while_its_agent_runs_asks_the_plugin_to_stop_it() {
    let fixture = Fixture::new();
    let run = run_native(
        &fixture,
        Review::Optional,
        3,
        Plugin::CancelsAfterBound,
        fixes_at_once,
        Some(prices()),
        |_| {},
    );
    assert!(
        matches!(run.outcome.terminal, Terminal::Cancelled { .. }),
        "expected a cancelled run, got {:?}",
        run.outcome
    );
    assert!(
        run.seen()
            .iter()
            .any(|seen| matches!(seen, Seen::Stop { agent_id, .. } if agent_id == "ag1")),
        "the agent was asked to stop: {:?}",
        run.seen()
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// The mock backend is only a scripted agent: a run on it books what a
/// native run books, a dispatch row `native_run` and an estimated cost.
#[test]
fn a_run_on_the_mock_backend_books_native_rows_and_estimated_usage() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let machine = fixture.machine_for(&repo);
    let gate = LocalGate::new(ConcurrencyLimits::default());
    let backend = MockBackend::new(move |spec| {
        // The reviewer's tree has no such file to remove.
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).ok();
        MockOutcome {
            result_text: Some("DONE\nFINDINGS: none".into()),
            exit_code: Some(0),
            usage: Some(crate::backend::UsageReport {
                cost: crate::backend::Cost::Estimated {
                    micros: MicroUsd::from_micros(7),
                },
                ..Default::default()
            }),
            ..Default::default()
        }
    });
    let outcome = fixture.execute_managed(
        &fixture.contract(Review::Required),
        &repo,
        &machine,
        &backend,
        &gate,
    );
    assert!(matches!(outcome.terminal, Terminal::Accepted(_)));
    let rows = dispatch_rows(&fixture, &outcome.run_id);
    assert_eq!(rows, vec![("native_run".to_string(), None)]);
    let conn = ledger_of(&fixture);
    let kinds: Vec<String> = conn
        .prepare("SELECT DISTINCT cost_kind FROM usage_events WHERE run_id = ?1")
        .expect("prepare")
        .query_map([outcome.run_id.as_str()], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    assert!(kinds[0].contains("EstimatedApiEquivalent"), "{kinds:?}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}
