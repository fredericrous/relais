//! The native launch of a dispatch (SPEC §23).
//!
//! `relais run` does not start the worker, the reviewer or the planner: it
//! asks the relais plugin of the parent Claude Code session to spawn it as a
//! subagent of its kind, so
//! Claude Code renders it as its own. This backend writes that request as a protocol
//! line (SPEC §29), registers it with the coordinator, and waits for the
//! plugin's `relais native bound` and `relais native stopped` calls, which
//! carry the agent and what it spent. The engine then judges the attempt as
//! it judges any other. Every dispatch is native: relais starts no
//! `claude` process of its own but the probes of the harness.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::admission::{
    AgentStatus, AgentUsage, Gate, GateError, NativeAsk, NativeProgress, NativeState,
    RegisterOutcome, StoppedReport, ADMISSION_POLL,
};
use crate::backend::{
    claims_blockage, Backend, BackendError, Capabilities, Cost, Harness, LaunchResult, LaunchSpec,
    UsageReport,
};
use crate::money::MicroUsd;
use crate::native::{agent_type, has_definition, kind_word, plugin_agent_type, HELLO_FRESH};
use crate::orchestration::{
    parse_transcript, price, unpriced_reason, CacheWrites, PriceTable, Speed, UsageRecord,
};
use crate::policy::EffortId;
use crate::procs::Ended;
use crate::protocol::{AgentKind, Request, Wire};

/// The label a worker that was never spawned ends with.
const SPAWN_MISSING: &str = "native_spawn_missing";

/// The label an attempt whose model has no shipped agent definition of its
/// kind ends with. The label keeps the name it has had since workers were
/// the only native dispatch.
const WORKER_MISSING: &str = "native_worker_missing";

/// The label an attempt ends with when the session's plugin stopped saying hello.
const MOD_GONE: &str = "mod_gone";

/// Consecutive unanswered status polls that end the wait: a coordinator
/// that has stopped answering cannot tell this attempt anything.
const STATUS_FAILURES_ALLOWED: u32 = 3;

/// How long the backend waits on the plugin.
#[derive(Debug, Clone, Copy)]
pub struct Waits {
    /// A request nobody bound within this ends `native_spawn_missing`.
    pub spawn: Duration,
    /// A session whose plugin said no hello for this long ends the attempt
    /// `mod_gone`.
    pub hello_lapse: Duration,
}

impl Waits {
    pub const DEFAULT: Self = Self {
        spawn: Duration::from_secs(120),
        hello_lapse: HELLO_FRESH,
    };
}

/// Where the backend's run talks to the plugin, and where it can see what
/// the plugin's agents wrote.
pub struct Link {
    pub run_id: String,
    pub session_id: String,
    pub wire: Wire,
    /// Claude Code's `projects` directory, where an agent's transcript is
    /// read for its message ids; `None` when it cannot be known.
    pub projects_dir: Option<PathBuf>,
}

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
/// or an agent never learned of is a fresh spawn. Only a worker is repaired:
/// a reviewer or a planner always spawns fresh.
fn route_for(known: Option<&KnownAgent>, spec: &LaunchSpec) -> Route {
    match (spec.agent, known) {
        (AgentKind::Worker, Some(agent)) if agent.model == spec.model => Route::Continue {
            agent_id: agent.agent_id.clone(),
            effort_note: effort_note(agent, spec),
        },
        (AgentKind::Worker, Some(_) | None) | (AgentKind::Reviewer | AgentKind::Planner, _) => {
            Route::Spawn
        }
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
            if has_definition(spec.agent, &spec.model, effort) {
                return None;
            }
            Some(format!(
                "{WORKER_MISSING}: relais ships no native {} for model {} at effort {}; \
                 use a model alias (haiku, sonnet, opus, fable)",
                kind_word(spec.agent),
                spec.model,
                effort.unwrap_or("default")
            ))
        }
    }
}

/// The request registered with the coordinator for this attempt.
fn ask_for(route: &Route, spec: &LaunchSpec) -> NativeAsk {
    match route {
        Route::Spawn => NativeAsk::Spawn {
            subagent_type: plugin_agent_type(&agent_type(
                spec.agent,
                &spec.model,
                spec.effort.as_ref().map(EffortId::as_str),
            )),
            model: spec.model.clone(),
            prompt: spec.prompt.clone(),
            worktree: spec.work_dir.clone(),
        },
        Route::Continue { agent_id, .. } => NativeAsk::Continue {
            agent_id: agent_id.clone(),
            message: spec.prompt.clone(),
        },
    }
}

/// The protocol line that asks the plugin for what was registered.
fn request_for<'a>(run: &'a str, spec: &'a LaunchSpec, ask: &'a NativeAsk) -> Request<'a> {
    match ask {
        NativeAsk::Spawn {
            subagent_type,
            model,
            prompt,
            worktree,
        } => Request::Spawn {
            run,
            dispatch: &spec.dispatch_id,
            agent_kind: spec.agent,
            subagent_type,
            model,
            description: format!("relais {}", spec.dispatch_id),
            prompt,
            cwd: worktree.to_string_lossy().into_owned(),
        },
        NativeAsk::Continue { agent_id, message } => Request::Continue {
            run,
            dispatch: &spec.dispatch_id,
            agent: agent_id,
            message,
        },
    }
}

/// How the wait for a subagent ended.
#[derive(Debug)]
enum Awaited {
    Stopped(StoppedReport),
    /// Anything but a stop: the attempt has no result.
    Ended {
        ended: Ended,
        detail: String,
    },
}

/// What a dispatch's reported usage adds up to.
#[derive(Debug, PartialEq)]
struct Booked {
    usage: UsageReport,
    effective_model: Option<String>,
    /// Why records could not be priced, one line per model and reason,
    /// in order: what the runner's backstop names.
    unpriced: Vec<String>,
}

/// Usage of the records given: tokens summed, cost estimated from the
/// machine's price table, the model of the last one. No record, no table or
/// a model without a price leaves the figure unknown, never zero.
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

/// What the plugin reported for a dispatch, priced as a transcript's
/// records are. A report without usage, or without the model that ran,
/// leaves the figure unknown, never zero.
fn usage_of_report(usage: Option<&AgentUsage>, prices: Option<&PriceTable>) -> Booked {
    let Some(usage) = usage else {
        return usage_of(&[], prices);
    };
    let Some(model) = usage.model.clone() else {
        return Booked {
            usage: UsageReport::unknown(),
            effective_model: None,
            unpriced: vec!["the agent reported no model".to_string()],
        };
    };
    // The plugin sums one cache-write figure over the turns, without the
    // tier it was written at: priced at the 5-minute rate, the default.
    let record = UsageRecord {
        message_id: String::new(),
        model,
        speed: Speed::Standard,
        input_tokens: usage.input_tokens.unwrap_or(0),
        output_tokens: usage.output_tokens.unwrap_or(0),
        cache_read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0),
        cache_writes: CacheWrites {
            ephemeral_5m_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
            ephemeral_1h_input_tokens: 0,
        },
        timestamp: String::new(),
    };
    usage_of(std::slice::from_ref(&record), prices)
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
        failure_detail: Some(detail),
        booked_message_ids: Vec::new(),
        unpriced: Vec::new(),
    }
}

/// Launches every dispatch as a native subagent of the parent session. The
/// harness it was given is only probed, for its identity and capabilities.
pub struct NativeBackend<'a> {
    harness: &'a dyn Harness,
    gate: &'a (dyn Gate + Sync),
    link: Link,
    waits: Waits,
    prices: Option<PriceTable>,
    memory: Mutex<Memory>,
}

impl<'a> NativeBackend<'a> {
    pub fn new(
        harness: &'a dyn Harness,
        gate: &'a (dyn Gate + Sync),
        link: Link,
        waits: Waits,
        prices: Option<PriceTable>,
    ) -> Self {
        Self {
            harness,
            gate,
            link,
            waits,
            prices,
            memory: Mutex::new(Memory::default()),
        }
    }

    /// Write one request line, so the plugin sees it now.
    fn publish(&self, request: &Request<'_>) -> Result<(), BackendError> {
        self.link
            .wire
            .send(request)
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
        // The agent the last poll found the dispatch bound to: once the run
        // is cancelled the coordinator settles the dispatch, and its record
        // no longer names the agent.
        let mut running: Option<String> = None;
        loop {
            if spec
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
            {
                if let Some(agent) = &running {
                    self.stop_agent(spec, agent);
                }
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
            match self.poll(spec) {
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
                Ok((progress, hello_age)) => {
                    failures = 0;
                    if let NativeProgress::Known {
                        state: NativeState::Bound { agent_id },
                    } = &progress
                    {
                        running = Some(agent_id.clone());
                    }
                    if let Some(awaited) = self.settled(spec, route, &progress, started) {
                        return awaited;
                    }
                    let alive = hello_age.is_some_and(|age| age < self.waits.hello_lapse);
                    if !alive {
                        return Awaited::Ended {
                            ended: Ended::Exited(1),
                            detail: format!(
                                "{MOD_GONE}: the relais plugin of session {} said no hello for \
                                 {}s while dispatch {} ran",
                                self.link.session_id,
                                self.waits.hello_lapse.as_secs(),
                                spec.dispatch_id
                            ),
                        };
                    }
                }
            }
            std::thread::sleep(ADMISSION_POLL);
        }
    }

    /// What the coordinator knows now: the dispatch's record, and how long
    /// ago the session's plugin last said hello.
    fn poll(&self, spec: &LaunchSpec) -> Result<(NativeProgress, Option<Duration>), GateError> {
        let progress = self.gate.native_status(&spec.dispatch_id)?;
        let hello_age = self.gate.native_hello_age(&self.link.session_id)?;
        Ok((progress, hello_age))
    }

    /// Ask the plugin to stop the agent of a cancelled attempt.
    fn stop_agent(&self, spec: &LaunchSpec, agent: &str) {
        let stop = Request::Stop {
            run: &self.link.run_id,
            dispatch: &spec.dispatch_id,
            agent,
        };
        // Dropped on failure: the attempt ends cancelled either way, and a
        // plugin that cannot be told has nothing left to stop.
        let _ = self.publish(&stop);
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
                    status,
                    usage,
                    answer,
                } => Some(Awaited::Stopped(StoppedReport {
                    agent_id: agent_id.clone(),
                    status: *status,
                    usage: usage.clone(),
                    answer: answer.clone(),
                })),
                NativeState::Failed { reason } => Some(Awaited::Ended {
                    ended: Ended::Exited(1),
                    detail: reason.clone(),
                }),
                NativeState::Requested { .. } if started.elapsed() >= self.waits.spawn => {
                    let nobody = match route {
                        Route::Spawn => "no session spawned it",
                        Route::Continue { .. } => "no session sent the continuation",
                    };
                    Some(Awaited::Ended {
                        ended: Ended::Exited(1),
                        detail: format!(
                            "{SPAWN_MISSING}: {nobody} within {}s of the request for dispatch {}",
                            self.waits.spawn.as_secs(),
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

    /// The message ids of an agent's transcript, found at
    /// `<projects>/*/<session>/subagents/agent-<agent>.jsonl`. They are
    /// read for nothing else: an older relais's usage import skips what is
    /// recorded here, so the agent's usage is not booked twice after a
    /// revert. Empty when the file is missing or holds no message.
    fn rollback_ids(&self, agent_id: &str) -> Vec<String> {
        let Some(projects) = &self.link.projects_dir else {
            return Vec::new();
        };
        // An unreadable directory is the same fact as a missing file: no
        // ids, which the run says as `rollback_ids_missing`.
        let Ok(slugs) = std::fs::read_dir(projects) else {
            return Vec::new();
        };
        let file = format!("agent-{agent_id}.jsonl");
        slugs
            .flatten()
            .find_map(|slug| {
                let path = slug
                    .path()
                    .join(&self.link.session_id)
                    .join("subagents")
                    .join(&file);
                std::fs::read_to_string(path).ok()
            })
            .map(|content| {
                parse_transcript(&content)
                    .into_iter()
                    .map(|record| record.message_id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The result of a subagent that stopped: its answer and what it
    /// reported spending.
    fn stopped_result(
        &self,
        spec: &LaunchSpec,
        report: StoppedReport,
        mut notes: Vec<String>,
    ) -> LaunchResult {
        let StoppedReport {
            agent_id,
            status,
            usage,
            answer,
        } = report;
        let booked = usage_of_report(usage.as_ref(), self.prices.as_ref());
        let failure = match status {
            AgentStatus::Completed => None,
            AgentStatus::Failed => Some("failed"),
            AgentStatus::Killed => Some("killed"),
        }
        .map(|how| format!("the native agent {agent_id} {how}"));
        if failure.is_none() && spec.agent == AgentKind::Worker {
            // A continuation runs at the effort its agent was spawned with,
            // whatever this attempt asked for, so the agent already on
            // record keeps its effort; only a fresh spawn records the
            // effort it was asked to run at. An agent that failed is not
            // continued: the next attempt spawns afresh.
            let mut memory = self.memory();
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
        }
        let result_text = answer.filter(|_| failure.is_none());
        if failure.is_some() {
            notes.clear();
        }
        LaunchResult {
            dispatch_id: spec.dispatch_id.clone(),
            ended: Ended::Exited(i32::from(failure.is_some())),
            stdout: String::new(),
            stderr: notes.join("\n"),
            worker_claims_blockage: result_text.as_deref().is_some_and(claims_blockage),
            result_text,
            effective_model: booked.effective_model,
            usage: booked.usage,
            failure_detail: failure,
            booked_message_ids: self.rollback_ids(&agent_id),
            session_id: Some(agent_id),
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
            .register_native(&self.link.session_id, &spec.dispatch_id, &ask)
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
        self.publish(&request_for(&self.link.run_id, spec, &ask))?;
        match self.await_stop(spec, &route) {
            Awaited::Stopped(report) => {
                let notes = match route {
                    Route::Continue {
                        effort_note: Some(note),
                        ..
                    } => vec![note],
                    Route::Continue { .. } | Route::Spawn => Vec::new(),
                };
                Ok(self.stopped_result(spec, report, notes))
            }
            Awaited::Ended { ended, detail } => Ok(ended_result(spec, ended, detail)),
        }
    }
}

impl Harness for NativeBackend<'_> {
    /// The harness a native agent runs on is the probed one: it is still
    /// Claude Code, so a run's recorded identity does not change.
    fn name(&self) -> &'static str {
        self.harness.name()
    }

    fn probe(&self) -> Option<Capabilities> {
        self.harness.probe()
    }
}

impl Backend for NativeBackend<'_> {
    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        self.launch_native(spec)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::orchestration::ModelPrice;

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
            tools: crate::backend::ToolSet::ModeDefault,
            agent: AgentKind::Worker,
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

    fn backend_in<'a>(
        harness: &'a dyn Harness,
        gate: &'a (dyn Gate + Sync),
        projects_dir: Option<PathBuf>,
    ) -> NativeBackend<'a> {
        NativeBackend::new(
            harness,
            gate,
            Link {
                run_id: "run-1".into(),
                session_id: "s1".into(),
                wire: Wire::to(std::io::sink()),
                projects_dir,
            },
            Waits::DEFAULT,
            None,
        )
    }

    fn reported(agent: &str, status: AgentStatus, usage: Option<AgentUsage>) -> StoppedReport {
        StoppedReport {
            agent_id: agent.into(),
            status,
            usage,
            answer: Some("DONE".into()),
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
            let agent_type = crate::native::worker_agent_type(&model, effort.as_deref());
            assert!(
                files.contains(&format!("agents/{agent_type}.md")),
                "{agent_type}"
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
    fn a_spawn_line_names_the_plugin_agent_the_worktree_and_carries_no_marker() {
        let spec = spec("sonnet", Some("medium"));
        let ask = ask_for(&Route::Spawn, &spec);
        let line = serde_json::to_string(&request_for("run-1", &spec, &ask)).expect("json");
        let input: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(input["relais"], "spawn");
        assert_eq!(input["run"], "run-1");
        assert_eq!(input["dispatch"], "d1");
        assert_eq!(input["agent_kind"], "worker");
        assert_eq!(input["subagent_type"], "relais:relais-worker-sonnet-medium");
        assert_eq!(input["model"], "sonnet");
        assert_eq!(input["description"], "relais d1");
        assert_eq!(input["cwd"], "/trees/task");
        assert_eq!(input["prompt"], "do the work");
        assert!(!line.contains("relais-dispatch"), "{line}");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn a_reviewer_and_a_planner_spawn_as_their_own_kind_and_are_never_continued() {
        let known = agent("sonnet", Some("medium"));
        for (kind, word) in [
            (AgentKind::Reviewer, "reviewer"),
            (AgentKind::Planner, "planner"),
        ] {
            let spec = LaunchSpec {
                agent: kind,
                ..spec("sonnet", Some("medium"))
            };
            assert_eq!(route_for(Some(&known), &spec), Route::Spawn);
            let ask = ask_for(&Route::Spawn, &spec);
            let line = serde_json::to_string(&request_for("run-1", &spec, &ask)).expect("json");
            let input: serde_json::Value = serde_json::from_str(&line).expect("json");
            assert_eq!(input["agent_kind"], word);
            assert_eq!(
                input["subagent_type"],
                format!("relais:relais-{word}-sonnet-medium")
            );
        }
    }

    #[test]
    fn a_refusal_names_the_kind_that_has_no_definition() {
        let reviewer = LaunchSpec {
            agent: AgentKind::Reviewer,
            ..spec("claude-sonnet-5-5", None)
        };
        let refusal = missing_definition(&Route::Spawn, &reviewer).expect("not shipped");
        assert!(
            refusal.contains("relais ships no native reviewer for model claude-sonnet-5-5"),
            "{refusal}"
        );
    }

    #[test]
    fn a_continue_line_names_the_agent_and_the_dispatch() {
        let spec = spec("sonnet", None);
        let route = Route::Continue {
            agent_id: "a1".into(),
            effort_note: None,
        };
        let line = serde_json::to_string(&request_for("run-1", &spec, &ask_for(&route, &spec)))
            .expect("json");
        let input: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(input["relais"], "continue");
        assert_eq!(input["dispatch"], "d1");
        assert_eq!(input["agent"], "a1");
        assert_eq!(input["message"], "do the work");
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

    #[test]
    fn reported_usage_is_priced_and_names_the_model_that_ran() {
        let usage = AgentUsage {
            input_tokens: Some(10),
            output_tokens: Some(5),
            cache_read_input_tokens: Some(0),
            cache_creation_input_tokens: Some(2),
            model: Some("sonnet-x".into()),
        };
        let booked = usage_of_report(Some(&usage), Some(&table()));
        assert_eq!(booked.usage.input_tokens, Some(10));
        assert_eq!(booked.usage.cache_write_tokens, Some(2));
        assert_eq!(
            booked.usage.cost,
            Cost::Estimated {
                micros: MicroUsd::from_micros(10 + 10)
            }
        );
        assert_eq!(booked.effective_model.as_deref(), Some("sonnet-x"));
        let unreported = usage_of_report(None, Some(&table()));
        assert_eq!(unreported.usage, UsageReport::unknown());
        let modelless = usage_of_report(Some(&AgentUsage::default()), Some(&table()));
        assert_eq!(modelless.usage.cost, Cost::Unknown);
        assert_eq!(modelless.effective_model, None);
        assert_eq!(modelless.unpriced.len(), 1);
    }

    /// Repairs raise effort on the same model by default, and a
    /// continuation keeps the effort its agent was spawned with: after two
    /// raised repairs, the note still names the effort the agent runs at.
    #[test]
    fn the_agent_keeps_its_spawn_effort_across_continuations() {
        let harness = crate::adapter::mock::MockBackend::new(|_| panic!("no launch"));
        let gate = crate::admission::LocalGate::new(crate::policy::ConcurrencyLimits::default());
        let backend = backend_in(&harness, &gate, None);
        let stop = |asked: &str| {
            backend.stopped_result(
                &spec("sonnet", Some(asked)),
                reported("a1", AgentStatus::Completed, None),
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
    }

    #[test]
    fn a_failed_or_killed_agent_has_no_result_and_is_not_continued() {
        let harness = crate::adapter::mock::MockBackend::new(|_| panic!("no launch"));
        let gate = crate::admission::LocalGate::new(crate::policy::ConcurrencyLimits::default());
        let backend = backend_in(&harness, &gate, None);
        for (status, word) in [
            (AgentStatus::Failed, "failed"),
            (AgentStatus::Killed, "killed"),
        ] {
            let result = backend.stopped_result(
                &spec("sonnet", None),
                reported("a1", status, None),
                Vec::new(),
            );
            assert_eq!(result.ended, Ended::Exited(1));
            assert_eq!(result.result_text, None);
            assert!(
                result
                    .failure_detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains(word)),
                "{result:?}"
            );
        }
        assert!(backend.memory().agents.is_empty());
    }

    /// The ids are those of the agent's transcript, found by session and
    /// agent under any project slug; a missing file has none.
    #[test]
    fn an_agents_transcript_gives_its_message_ids() {
        let harness = crate::adapter::mock::MockBackend::new(|_| panic!("no launch"));
        let gate = crate::admission::LocalGate::new(crate::policy::ConcurrencyLimits::default());
        let dir = crate::test_support::short_temp_dir("native-ids");
        let subagents = dir.join("-slug").join("s1").join("subagents");
        std::fs::create_dir_all(&subagents).expect("dirs");
        let line = |id: &str| {
            format!(
                r#"{{"type":"assistant","timestamp":"2026-10-06T10:00:00Z","message":{{"id":"{id}","model":"sonnet-x","usage":{{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation":{{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0}}}}}}}}"#
            )
        };
        std::fs::write(
            subagents.join("agent-a1.jsonl"),
            format!("{}\n{}\n", line("m1"), line("m2")),
        )
        .expect("written");
        let backend = backend_in(&harness, &gate, Some(dir.to_path_buf()));
        assert_eq!(backend.rollback_ids("a1"), ["m1", "m2"]);
        assert!(backend.rollback_ids("a2").is_empty());
        let nowhere = backend_in(&harness, &gate, Some(dir.join("absent")));
        assert!(nowhere.rollback_ids("a1").is_empty());
        let unknown = backend_in(&harness, &gate, None);
        assert!(unknown.rollback_ids("a1").is_empty());
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
