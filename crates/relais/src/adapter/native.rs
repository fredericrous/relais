//! The native presentation of a worker attempt (SPEC §23).
//!
//! `relais run --native` does not start the worker: it asks the parent
//! Claude Code session to spawn it as a subagent, so Claude Code renders it
//! as its own. This backend publishes that request on stdout, registers it
//! with the coordinator, whose hook makes the session run exactly what was
//! registered, and waits for the subagent to stop. It then reads what the
//! subagent spent from its transcript, so the engine judges the attempt as
//! it judges a headless one. Every launch that is not a native worker's
//! goes to the headless backend untouched.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::admission::{
    Gate, NativeAsk, NativeProgress, NativeState, RegisterOutcome, ADMISSION_POLL,
};
use crate::backend::{
    claims_blockage, Backend, BackendError, Capabilities, Cost, LaunchResult, LaunchSpec,
    Presentation, UsageReport,
};
use crate::money::MicroUsd;
use crate::native::{has_worker_definition, marker_line, worker_agent_type};
use crate::orchestration::{parse_transcript, price, unpriced_reason, PriceTable, UsageRecord};
use crate::policy::EffortId;
use crate::procs::Ended;

/// The label a worker that was never spawned ends with.
const SPAWN_MISSING: &str = "native_spawn_missing";

/// The label an attempt whose model has no shipped worker definition ends with.
const WORKER_MISSING: &str = "native_worker_missing";

/// Consecutive unanswered status polls that end the wait: a coordinator
/// that has stopped answering cannot tell this attempt anything.
const STATUS_FAILURES_ALLOWED: u32 = 3;

/// The subagent a task's last native attempt ran as.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KnownAgent {
    agent_id: String,
    model: String,
    effort: Option<EffortId>,
}

/// What this run remembers between native attempts.
#[derive(Debug, Default)]
struct Memory {
    /// The agent of each task's last native attempt, by the task's tree.
    agents: BTreeMap<PathBuf, KnownAgent>,
    /// Transcript message ids already booked to an earlier attempt.
    booked: BTreeSet<String>,
}

/// How an attempt reaches the session.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    Spawn,
    /// Send the prompt to the agent that ran the last attempt. `effort_note`
    /// is set when the attempt asked for another effort than the agent has.
    Continue {
        agent_id: String,
        effort_note: Option<String>,
    },
}

/// A repair continues the agent that bound for the last attempt as long as
/// the model is the same; an escalation to another model, a first attempt,
/// or an agent never learned of is a fresh spawn.
fn route_for(known: Option<&KnownAgent>, spec: &LaunchSpec) -> Route {
    match known {
        Some(agent) if agent.model == spec.model => Route::Continue {
            agent_id: agent.agent_id.clone(),
            effort_note: effort_note(agent, spec),
        },
        Some(_) | None => Route::Spawn,
    }
}

fn effort_note(agent: &KnownAgent, spec: &LaunchSpec) -> Option<String> {
    let asked = spec.effort.as_ref()?;
    if agent.effort.as_ref() == Some(asked) {
        return None;
    }
    let kept = agent
        .effort
        .as_ref()
        .map_or("its default", EffortId::as_str);
    Some(format!(
        "the continuation keeps agent {}'s own effort ({kept}); the attempt asked for {asked}",
        agent.agent_id
    ))
}

/// Why a spawn cannot be asked for: relais ships no definition for the
/// attempt's (model, effort). A continuation names an agent that already
/// exists, so it needs none.
fn missing_definition(route: &Route, spec: &LaunchSpec) -> Option<String> {
    match route {
        Route::Continue { .. } => None,
        Route::Spawn => {
            let effort = spec.effort.as_ref().map(EffortId::as_str);
            if has_worker_definition(&spec.model, effort) {
                return None;
            }
            Some(format!(
                "{WORKER_MISSING}: relais ships no native worker for model {} at effort {}; \
                 use a model alias (haiku, sonnet, opus, fable) or run without --native",
                spec.model,
                effort.unwrap_or("default")
            ))
        }
    }
}

/// The request registered with the coordinator for this attempt.
fn ask_for(route: &Route, spec: &LaunchSpec) -> NativeAsk {
    let text = format!("{}\n{}", spec.prompt, marker_line(&spec.dispatch_id));
    match route {
        Route::Spawn => NativeAsk::Spawn {
            subagent_type: worker_agent_type(
                &spec.model,
                spec.effort.as_ref().map(EffortId::as_str),
            ),
            model: spec.model.clone(),
            prompt: text,
            worktree: spec.work_dir.clone(),
        },
        Route::Continue { agent_id, .. } => NativeAsk::Continue {
            agent_id: agent_id.clone(),
            message: text,
        },
    }
}

/// The one stdout line that is the parent session's whole interface: its
/// JSON is exactly the tool input to send.
fn request_line(spec: &LaunchSpec, ask: &NativeAsk) -> String {
    match ask {
        NativeAsk::Spawn {
            subagent_type,
            model,
            prompt,
            ..
        } => format!(
            "RELAIS-SPAWN {}",
            serde_json::json!({
                "dispatch_id": spec.dispatch_id,
                "subagent_type": subagent_type,
                "model": model,
                "description": format!("relais {}", spec.dispatch_id),
                "prompt": prompt,
                "isolation": "worktree",
                "run_in_background": true,
            })
        ),
        NativeAsk::Continue { agent_id, message } => format!(
            "RELAIS-CONTINUE {}",
            serde_json::json!({
                "dispatch_id": spec.dispatch_id,
                "to": agent_id,
                "message": message,
            })
        ),
    }
}

/// How the wait for a subagent ended.
#[derive(Debug)]
enum Awaited {
    Stopped {
        agent_id: String,
        transcript_path: Option<PathBuf>,
        last_assistant_message: Option<String>,
    },
    /// Anything but a stop: the attempt has no result.
    Ended { ended: Ended, detail: String },
}

/// What the new records of a transcript add up to.
#[derive(Debug, PartialEq)]
struct Booked {
    usage: UsageReport,
    effective_model: Option<String>,
    /// Why records could not be priced, one line per model and reason,
    /// in transcript order: what the runner's backstop names.
    unpriced: Vec<String>,
}

/// Usage of the records not booked yet: tokens summed, cost estimated from
/// the machine's price table, the model of the last one. No new record, no
/// table or a model without a price leaves the figure unknown, never zero.
fn usage_of(records: &[UsageRecord], prices: Option<&PriceTable>) -> Booked {
    if records.is_empty() {
        return Booked {
            usage: UsageReport::unknown(),
            effective_model: None,
            unpriced: Vec::new(),
        };
    }
    let mut unpriced: Vec<String> = Vec::new();
    for record in records {
        let reason = match prices {
            Some(table) => unpriced_reason(record, table),
            None => Some(format!(
                "{}: this machine has no [pricing] table",
                record.model
            )),
        };
        if let Some(reason) = reason {
            if !unpriced.contains(&reason) {
                unpriced.push(reason);
            }
        }
    }
    let sum = |of: fn(&UsageRecord) -> u64| {
        let total: u64 = records.iter().map(of).sum();
        Some(i64::try_from(total).unwrap_or(i64::MAX))
    };
    let cost = prices
        .and_then(|table| {
            records
                .iter()
                .map(|record| price(record, table).cost)
                .try_fold(MicroUsd::ZERO, |total, cost| {
                    cost.map(|cost| total.saturating_add(cost))
                })
        })
        .map_or(Cost::Unknown, |micros| Cost::Estimated { micros });
    Booked {
        usage: UsageReport {
            input_tokens: sum(|record| record.input_tokens),
            output_tokens: sum(|record| record.output_tokens),
            cache_read_tokens: sum(|record| record.cache_read_input_tokens),
            cache_write_tokens: sum(|record| {
                record.cache_writes.ephemeral_5m_input_tokens
                    + record.cache_writes.ephemeral_1h_input_tokens
            }),
            cost,
        },
        effective_model: records.last().map(|record| record.model.clone()),
        unpriced,
    }
}

/// A launch that did not reach a result.
fn ended_result(spec: &LaunchSpec, ended: Ended, detail: String) -> LaunchResult {
    LaunchResult {
        dispatch_id: spec.dispatch_id.clone(),
        ended,
        stdout: String::new(),
        stderr: String::new(),
        result_text: None,
        session_id: None,
        effective_model: None,
        usage: UsageReport::unknown(),
        worker_claims_blockage: false,
        permission_denials: Vec::new(),
        failure_detail: Some(detail),
        booked_message_ids: Vec::new(),
        unpriced: Vec::new(),
    }
}

/// Launches a worker attempt as a native subagent of the parent session,
/// and everything else on the headless backend it wraps.
pub struct NativeBackend<'a> {
    headless: &'a dyn Backend,
    gate: &'a (dyn Gate + Sync),
    session_id: String,
    spawn_wait: Duration,
    prices: Option<PriceTable>,
    memory: Mutex<Memory>,
    /// Where the request lines go: stdout, which the parent session reads.
    out: Mutex<Box<dyn Write + Send + 'a>>,
}

impl<'a> NativeBackend<'a> {
    pub fn new(
        headless: &'a dyn Backend,
        gate: &'a (dyn Gate + Sync),
        session_id: String,
        spawn_wait: Duration,
        prices: Option<PriceTable>,
    ) -> Self {
        Self::writing_to(
            headless,
            gate,
            session_id,
            spawn_wait,
            prices,
            Box::new(std::io::stdout()),
        )
    }

    /// The same backend writing its request lines to `out` instead of
    /// stdout: the port a test reads the parent session's side of.
    pub fn writing_to(
        headless: &'a dyn Backend,
        gate: &'a (dyn Gate + Sync),
        session_id: String,
        spawn_wait: Duration,
        prices: Option<PriceTable>,
        out: Box<dyn Write + Send + 'a>,
    ) -> Self {
        Self {
            headless,
            gate,
            session_id,
            spawn_wait,
            prices,
            memory: Mutex::new(Memory::default()),
            out: Mutex::new(out),
        }
    }

    /// Write one request line and flush it, so the session sees it now.
    fn publish(&self, line: &str) -> Result<(), BackendError> {
        // A poisoned lock only means another writer panicked mid-line;
        // the next line is written whole.
        let mut out = self
            .out
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writeln!(out, "{line}")
            .and_then(|()| out.flush())
            .map_err(|e| BackendError::Launch(format!("the native request was not written: {e}")))
    }

    fn memory(&self) -> std::sync::MutexGuard<'_, Memory> {
        // A poisoned lock only means another attempt panicked: what it
        // remembered is still the best account there is.
        self.memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Wait until the registered request has run to its stop, or cannot.
    fn await_stop(&self, spec: &LaunchSpec, route: &Route) -> Awaited {
        let started = Instant::now();
        let mut failures = 0u32;
        loop {
            if spec
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
            {
                return Awaited::Ended {
                    ended: Ended::Cancelled,
                    detail: "the dispatch was cancelled while the native worker ran".into(),
                };
            }
            if started.elapsed() >= spec.wall_timeout {
                return Awaited::Ended {
                    ended: Ended::TimedOut,
                    detail: "the native worker did not stop within the attempt's wall time".into(),
                };
            }
            match self.gate.native_status(&spec.dispatch_id) {
                Err(e) => {
                    failures += 1;
                    if failures >= STATUS_FAILURES_ALLOWED {
                        return Awaited::Ended {
                            ended: Ended::Exited(1),
                            detail: format!(
                                "the coordinator did not answer {STATUS_FAILURES_ALLOWED} status \
                                 polls in a row ({e})"
                            ),
                        };
                    }
                }
                Ok(progress) => {
                    failures = 0;
                    if let Some(awaited) = self.settled(spec, route, &progress, started) {
                        return awaited;
                    }
                }
            }
            std::thread::sleep(ADMISSION_POLL);
        }
    }

    /// What one status says about the wait, `None` while it goes on.
    fn settled(
        &self,
        spec: &LaunchSpec,
        route: &Route,
        progress: &NativeProgress,
        started: Instant,
    ) -> Option<Awaited> {
        match progress {
            NativeProgress::Known { state } => match state {
                NativeState::Stopped {
                    agent_id,
                    transcript_path,
                    last_assistant_message,
                } => Some(Awaited::Stopped {
                    agent_id: agent_id.clone(),
                    transcript_path: transcript_path.clone(),
                    last_assistant_message: last_assistant_message.clone(),
                }),
                NativeState::Failed { reason } => Some(Awaited::Ended {
                    ended: Ended::Exited(1),
                    detail: reason.clone(),
                }),
                NativeState::Requested { .. } if started.elapsed() >= self.spawn_wait => {
                    let nobody = match route {
                        Route::Spawn => "no session spawned it",
                        Route::Continue { .. } => "no session sent the continuation",
                    };
                    Some(Awaited::Ended {
                        ended: Ended::Exited(1),
                        detail: format!(
                            "{SPAWN_MISSING}: {nobody} within {}s of the request for dispatch {}",
                            self.spawn_wait.as_secs(),
                            spec.dispatch_id
                        ),
                    })
                }
                NativeState::Requested { .. }
                | NativeState::Claimed
                | NativeState::TreeGiven { .. }
                | NativeState::Bound { .. } => None,
            },
            NativeProgress::Finished | NativeProgress::Unknown => Some(Awaited::Ended {
                ended: Ended::Exited(1),
                detail: format!(
                    "the coordinator lost native dispatch {}: it holds no record of it",
                    spec.dispatch_id
                ),
            }),
        }
    }

    /// The result of a subagent that stopped: its last message, and the
    /// usage of the transcript records no earlier attempt has booked.
    fn stopped_result(
        &self,
        spec: &LaunchSpec,
        agent_id: String,
        transcript_path: Option<&Path>,
        last_assistant_message: Option<String>,
        mut notes: Vec<String>,
    ) -> LaunchResult {
        let records = match transcript_path.map(std::fs::read_to_string) {
            Some(Ok(content)) => parse_transcript(&content),
            Some(Err(e)) => {
                notes.push(format!("the agent's transcript could not be read ({e})"));
                Vec::new()
            }
            None => {
                notes.push("the agent's stop named no transcript".into());
                Vec::new()
            }
        };
        let fresh: Vec<UsageRecord> = {
            let mut memory = self.memory();
            let fresh: Vec<UsageRecord> = records
                .into_iter()
                .filter(|record| !memory.booked.contains(&record.message_id))
                .collect();
            memory
                .booked
                .extend(fresh.iter().map(|record| record.message_id.clone()));
            // A continuation runs at the effort its agent was spawned with,
            // whatever this attempt asked for, so the agent already on
            // record keeps its effort; only a fresh spawn records the
            // effort it was asked to run at.
            let effort = match memory.agents.get(&spec.work_dir) {
                Some(known) if known.agent_id == agent_id => known.effort.clone(),
                Some(_) | None => spec.effort.clone(),
            };
            memory.agents.insert(
                spec.work_dir.clone(),
                KnownAgent {
                    agent_id: agent_id.clone(),
                    model: spec.model.clone(),
                    effort,
                },
            );
            fresh
        };
        let booked = usage_of(&fresh, self.prices.as_ref());
        LaunchResult {
            dispatch_id: spec.dispatch_id.clone(),
            ended: Ended::Exited(0),
            stdout: String::new(),
            stderr: notes.join("\n"),
            worker_claims_blockage: last_assistant_message
                .as_deref()
                .is_some_and(claims_blockage),
            result_text: last_assistant_message,
            session_id: Some(agent_id),
            effective_model: booked.effective_model,
            usage: booked.usage,
            permission_denials: Vec::new(),
            failure_detail: None,
            booked_message_ids: fresh.into_iter().map(|record| record.message_id).collect(),
            unpriced: booked.unpriced,
        }
    }

    fn launch_native(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        let known = self.memory().agents.get(&spec.work_dir).cloned();
        let route = route_for(known.as_ref(), spec);
        if let Some(refusal) = missing_definition(&route, spec) {
            return Ok(ended_result(spec, Ended::Exited(1), refusal));
        }
        let ask = ask_for(&route, spec);
        match self
            .gate
            .register_native(&self.session_id, &spec.dispatch_id, &ask)
        {
            Ok(RegisterOutcome::Registered) => {}
            Ok(refusal) => {
                return Ok(ended_result(
                    spec,
                    Ended::Exited(1),
                    format!("the coordinator refused the native dispatch: {refusal:?}"),
                ))
            }
            Err(e) => {
                return Ok(ended_result(
                    spec,
                    Ended::Exited(1),
                    format!("the native dispatch could not be registered: {e}"),
                ))
            }
        }
        self.publish(&request_line(spec, &ask))?;
        match self.await_stop(spec, &route) {
            Awaited::Stopped {
                agent_id,
                transcript_path,
                last_assistant_message,
            } => {
                let notes = match route {
                    Route::Continue {
                        effort_note: Some(note),
                        ..
                    } => vec![note],
                    Route::Continue { .. } | Route::Spawn => Vec::new(),
                };
                Ok(self.stopped_result(
                    spec,
                    agent_id,
                    transcript_path.as_deref(),
                    last_assistant_message,
                    notes,
                ))
            }
            Awaited::Ended { ended, detail } => Ok(ended_result(spec, ended, detail)),
        }
    }
}

impl Backend for NativeBackend<'_> {
    /// The harness the worker runs on is the wrapped one: a native worker
    /// is still Claude Code, so a run's recorded identity does not change.
    fn name(&self) -> &'static str {
        self.headless.name()
    }

    fn probe(&self) -> Option<Capabilities> {
        self.headless.probe()
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        match spec.presentation {
            Presentation::Headless => self.headless.launch(spec),
            Presentation::Native => self.launch_native(spec),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{CacheWrites, ModelPrice, Speed};

    fn effort(name: &str) -> EffortId {
        EffortId::parse(name).expect("a valid effort identifier")
    }

    fn spec(model: &str, effort_name: Option<&str>) -> LaunchSpec {
        LaunchSpec {
            dispatch_id: "d1".into(),
            prompt: "do the work".into(),
            model: model.into(),
            effort: effort_name.map(effort),
            max_turns: None,
            budget_micros: None,
            disallowed_tools: Vec::new(),
            allowed_tools: Vec::new(),
            work_dir: PathBuf::from("/trees/task"),
            env: crate::backend::LaunchEnv::default(),
            wall_timeout: Duration::from_secs(60),
            cancel: None,
            pid_slot: None,
            sandbox: None,
            tools: crate::backend::ToolSet::ModeDefault,
            presentation: Presentation::Native,
        }
    }

    fn agent(model: &str, effort_name: Option<&str>) -> KnownAgent {
        KnownAgent {
            agent_id: "a1".into(),
            model: model.into(),
            effort: effort_name.map(effort),
        }
    }

    fn record(id: &str, model: &str, input: u64, output: u64) -> UsageRecord {
        UsageRecord {
            message_id: id.into(),
            model: model.into(),
            speed: Speed::Standard,
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: 0,
            cache_writes: CacheWrites {
                ephemeral_5m_input_tokens: 3,
                ephemeral_1h_input_tokens: 4,
            },
            timestamp: "2026-10-06T00:00:00Z".into(),
        }
    }

    fn table() -> PriceTable {
        PriceTable {
            version: "t".into(),
            models: vec![ModelPrice {
                ids: vec!["sonnet-x".into()],
                input: 1_000_000,
                output: 2_000_000,
                cache_read: 0,
                cache_write_5m: 0,
                cache_write_1h: 0,
                fast_input: None,
                fast_output: None,
            }],
        }
    }

    #[test]
    fn the_same_model_continues_and_anything_else_spawns() {
        let known = agent("sonnet", Some("medium"));
        assert_eq!(
            route_for(Some(&known), &spec("sonnet", Some("medium"))),
            Route::Continue {
                agent_id: "a1".into(),
                effort_note: None
            }
        );
        assert_eq!(
            route_for(Some(&known), &spec("opus", Some("medium"))),
            Route::Spawn
        );
        assert_eq!(route_for(None, &spec("sonnet", None)), Route::Spawn);
    }

    #[test]
    fn a_spawn_without_a_shipped_definition_is_refused_naming_model_and_effort() {
        let refusal = missing_definition(&Route::Spawn, &spec("claude-sonnet-5-5", Some("high")))
            .expect("not shipped");
        assert!(refusal.starts_with("native_worker_missing:"), "{refusal}");
        assert!(refusal.contains("claude-sonnet-5-5"), "{refusal}");
        assert!(refusal.contains("high"), "{refusal}");
        assert!(refusal.contains("haiku, sonnet, opus, fable"), "{refusal}");
        assert!(refusal.contains("without --native"), "{refusal}");
        assert!(missing_definition(&Route::Spawn, &spec("haiku", Some("high"))).is_some());
        assert!(missing_definition(&Route::Spawn, &spec("haiku", None)).is_none());
        assert!(missing_definition(&Route::Spawn, &spec("opus", Some("max"))).is_none());
    }

    #[test]
    fn every_name_a_shipped_pair_can_print_has_an_install_file() {
        let files: Vec<String> = crate::install::owned_files()
            .into_iter()
            .map(|(path, _)| path.to_string_lossy().replace('\\', "/"))
            .collect();
        for (model, effort) in crate::native::worker_agent_types() {
            let ask = ask_for(&Route::Spawn, &spec(&model, effort.as_deref()));
            let NativeAsk::Spawn { subagent_type, .. } = ask else {
                panic!("a spawn route asks for a spawn");
            };
            assert!(
                files.contains(&format!("agents/{subagent_type}.md")),
                "{subagent_type}"
            );
        }
    }

    #[test]
    fn a_continuation_that_asked_another_effort_says_the_agent_kept_its_own() {
        let known = agent("sonnet", Some("medium"));
        let Route::Continue { effort_note, .. } =
            route_for(Some(&known), &spec("sonnet", Some("high")))
        else {
            panic!("a continuation");
        };
        let note = effort_note.expect("a note");
        assert!(note.contains("medium") && note.contains("high"), "{note}");
    }

    #[test]
    fn a_spawn_line_is_the_tool_input_with_the_marker_in_the_prompt() {
        let spec = spec("sonnet", Some("medium"));
        let ask = ask_for(&Route::Spawn, &spec);
        let line = request_line(&spec, &ask);
        let json = line.strip_prefix("RELAIS-SPAWN ").expect("a spawn line");
        let input: serde_json::Value = serde_json::from_str(json).expect("json");
        assert_eq!(input["dispatch_id"], "d1");
        assert_eq!(input["subagent_type"], "relais-worker-sonnet-medium");
        assert_eq!(input["model"], "sonnet");
        assert_eq!(input["description"], "relais d1");
        assert_eq!(input["isolation"], "worktree");
        assert_eq!(input["run_in_background"], true);
        let prompt = input["prompt"].as_str().expect("prompt");
        assert!(prompt.starts_with("do the work\n"), "{prompt}");
        assert!(prompt.ends_with(&marker_line("d1")), "{prompt}");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn a_continue_line_names_the_agent_and_carries_the_marker() {
        let spec = spec("sonnet", None);
        let route = Route::Continue {
            agent_id: "a1".into(),
            effort_note: None,
        };
        let line = request_line(&spec, &ask_for(&route, &spec));
        let json = line
            .strip_prefix("RELAIS-CONTINUE ")
            .expect("a continue line");
        let input: serde_json::Value = serde_json::from_str(json).expect("json");
        assert_eq!(input["to"], "a1");
        assert!(input["message"]
            .as_str()
            .expect("message")
            .ends_with(&marker_line("d1")));
    }

    #[test]
    fn usage_sums_the_new_records_and_prices_them_as_an_estimate() {
        let records = [
            record("m1", "sonnet-x", 10, 5),
            record("m2", "sonnet-x", 20, 7),
        ];
        let booked = usage_of(&records, Some(&table()));
        assert_eq!(booked.usage.input_tokens, Some(30));
        assert_eq!(booked.usage.output_tokens, Some(12));
        assert_eq!(booked.usage.cache_write_tokens, Some(14));
        // 30 input at $1/M and 12 output at $2/M.
        assert_eq!(
            booked.usage.cost,
            Cost::Estimated {
                micros: MicroUsd::from_micros(30 + 24)
            }
        );
        assert_eq!(booked.effective_model.as_deref(), Some("sonnet-x"));
    }

    /// Repairs raise effort on the same model by default, and a
    /// continuation keeps the effort its agent was spawned with: after two
    /// raised repairs, the note still names the effort the agent runs at.
    #[test]
    fn the_agent_keeps_its_spawn_effort_across_continuations() {
        let headless = crate::adapter::mock::MockBackend::new(|_| panic!("no headless launch"));
        let gate = crate::admission::LocalGate::new(crate::policy::ConcurrencyLimits::default());
        let backend = NativeBackend::writing_to(
            &headless,
            &gate,
            "s1".into(),
            Duration::from_secs(1),
            None,
            Box::new(std::io::sink()),
        );
        let stop = |asked: &str| {
            backend.stopped_result(
                &spec("sonnet", Some(asked)),
                "a1".into(),
                None,
                None,
                Vec::new(),
            )
        };
        stop("medium");
        let route_for_ask = |asked: &str| {
            let known = backend
                .memory()
                .agents
                .get(Path::new("/trees/task"))
                .cloned();
            route_for(known.as_ref(), &spec("sonnet", Some(asked)))
        };
        assert!(matches!(
            route_for_ask("high"),
            Route::Continue { effort_note: Some(ref note), .. } if note.contains("(medium)")
        ));
        stop("high");
        assert!(matches!(
            route_for_ask("xhigh"),
            Route::Continue { effort_note: Some(ref note), .. } if note.contains("(medium)")
        ));
        stop("xhigh");
        assert!(matches!(
            route_for_ask("high"),
            Route::Continue {
                effort_note: Some(_),
                ..
            }
        ));
    }

    /// The attempt names exactly the message ids it booked, and a
    /// continuation's attempt does not name the ones an earlier attempt took.
    #[test]
    fn a_stopped_attempt_names_the_message_ids_it_booked_once() {
        let headless = crate::adapter::mock::MockBackend::new(|_| panic!("no headless launch"));
        let gate = crate::admission::LocalGate::new(crate::policy::ConcurrencyLimits::default());
        let backend = NativeBackend::writing_to(
            &headless,
            &gate,
            "s1".into(),
            Duration::from_secs(1),
            None,
            Box::new(std::io::sink()),
        );
        let dir = crate::test_support::short_temp_dir("native-booked");
        let line = |id: &str| {
            format!(
                r#"{{"type":"assistant","timestamp":"2026-10-06T10:00:00Z","message":{{"id":"{id}","model":"sonnet-x","usage":{{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation":{{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0}}}}}}}}"#
            )
        };
        let transcript = dir.join("agent-a1.jsonl");
        std::fs::write(&transcript, format!("{}\n{}\n", line("m1"), line("m2"))).expect("written");
        let stop = || {
            backend.stopped_result(
                &spec("sonnet", None),
                "a1".into(),
                Some(&transcript),
                None,
                Vec::new(),
            )
        };
        assert_eq!(stop().booked_message_ids, ["m1", "m2"]);
        assert!(stop().booked_message_ids.is_empty());
    }

    /// The reasons are said record by record, never inferred from the
    /// total: a fast-mode record of a priced model lacks a fast rate, it
    /// does not lack an entry; and an unpriced model followed by a priced
    /// one is the one named.
    #[test]
    fn unpriced_records_are_named_with_their_own_reason() {
        let mut fast = record("m1", "sonnet-x", 10, 5);
        fast.speed = Speed::Other("fast".into());
        let booked = usage_of(&[fast], Some(&table()));
        assert_eq!(booked.usage.cost, Cost::Unknown);
        assert_eq!(
            booked.unpriced,
            vec![
                "sonnet-x's [pricing.models] entry has no fast-mode rate (fast_input, fast_output)"
                    .to_string()
            ]
        );
        let mixed = [record("m1", "other", 1, 1), record("m2", "sonnet-x", 10, 5)];
        let booked = usage_of(&mixed, Some(&table()));
        assert_eq!(booked.effective_model.as_deref(), Some("sonnet-x"));
        assert_eq!(
            booked.unpriced,
            vec!["other has no [pricing.models] entry".to_string()]
        );
        let priced = usage_of(&[record("m1", "sonnet-x", 10, 5)], Some(&table()));
        assert!(priced.unpriced.is_empty());
    }

    #[test]
    fn a_cost_without_a_price_is_unknown_never_zero() {
        let records = [record("m1", "sonnet-x", 10, 5)];
        assert_eq!(usage_of(&records, None).usage.cost, Cost::Unknown);
        let unpriced = [record("m1", "sonnet-x", 10, 5), record("m2", "other", 1, 1)];
        let booked = usage_of(&unpriced, Some(&table()));
        assert_eq!(booked.usage.cost, Cost::Unknown);
        assert_eq!(
            booked.usage.input_tokens,
            Some(11),
            "tokens are still known"
        );
        assert_eq!(usage_of(&[], Some(&table())).usage, UsageReport::unknown());
    }
}
