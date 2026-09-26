//! Wiring [`decide`](super::decide) to a live coordinator, a live
//! stdout and a journal (SPEC §23).
//!
//! [`handle`] is what `relais hook` runs for real: parse the payload,
//! ask the coordinator when — and only when — it names a spawn
//! [`decide`](super::decide::decide) can still refuse, apply the pure
//! decision, and release whatever a refused spawn's own request may
//! have reserved before the caller prints anything. This is the one
//! caller `decide.rs`'s module doc used to say did not exist yet.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use super::decide::{decide_or_silent, CoordinatorAnswer, HookAnswer};
use super::event::{self, HookEvent, ToolCallPhase};
use crate::admission::{
    Decision, DispatchRequest, DispatchSource, Gate, Provenance, Refusal, ResourceClass,
    RunRegistration,
};
use crate::ids;
use crate::policy::{HookAdmissionSettings, QueueBehaviour};
use crate::runner::ADMISSION_POLL;

/// What one hook firing did, end to end: the event a payload classified
/// as, whatever the coordinator answered (only ever `Some` for a
/// `PreToolUse` spawn), and the decision that came of it.
#[derive(Debug)]
pub struct Handled {
    pub event: HookEvent,
    pub coordinator: CoordinatorAnswer,
    pub answer: HookAnswer,
    /// How long [`ask_coordinator`] waited for a seat before this
    /// answer, via [`wait_for_a_seat`]. Zero for every firing that was
    /// never queued at all.
    pub waited: Duration,
}

/// Handle one hook payload: parse it, ask the coordinator about a
/// spawn, apply the existing pure decision, and then keep the seat's
/// record in step with the agent — the request itself withdrawn on a
/// refusal, the seat bound to the agent a spawn launched, and the seat
/// and the reservation given back once that agent has ended — so a
/// session's cap counts agents that are running rather than agents that
/// ever started.
///
/// Wrapped in `catch_unwind` for the same reason [`decide_or_silent`]
/// is: a hook that dies mid-answer would otherwise leave the tool call
/// it was watching unanswered, and this is the one place that reaches
/// a real socket and a real clock, neither of which `decide` itself
/// touches.
pub fn handle(payload: &[u8], settings: &HookAdmissionSettings, gate: &dyn Gate) -> Handled {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle_inner(payload, settings, gate)
    }))
    .unwrap_or(Handled {
        event: HookEvent::NotOurs,
        coordinator: None,
        answer: HookAnswer::Silent,
        waited: Duration::ZERO,
    })
}

fn handle_inner(payload: &[u8], settings: &HookAdmissionSettings, gate: &dyn Gate) -> Handled {
    let event = event::parse(payload);
    let (coordinator, waited) = ask_coordinator(&event, settings, gate);
    let answer = decide_or_silent(&event, settings, coordinator.clone(), waited);
    follow_agent_lifecycle(&event, gate);
    if matches!(answer, HookAnswer::Refuse { .. }) {
        if let HookEvent::AgentToolCall(call) = &event {
            let dispatch_id = ids::derive_dispatch_id(&call.session_id, &call.tool_use_id);
            // Best effort, and often a no-op: `decide` only refuses a
            // spawn the coordinator itself refused, and a refusal never
            // reserves anything (`admission::AdmissionState::request`
            // checks the hard limits before admitting). Called anyway
            // — a refusal that DID leave something held is exactly the
            // leak this guards against, and a hook cannot fail on the
            // cleanup call any more than on the decision itself.
            let _ = gate.withdraw(dispatch_id.as_str());
        }
    }
    Handled {
        event,
        coordinator,
        answer,
        waited,
    }
}

/// Keep a spawn's seat for exactly as long as the AGENT runs: admitted
/// on `PreToolUse`, bound to its agent on `PostToolUse`, given back on
/// `SubagentStop`.
///
/// The seat follows the agent, not the tool call, because the two end at
/// different moments. A real Claude Code 2.1.282 session showed an Agent
/// `PostToolUse` arriving ~100ms after its `PreToolUse` with
/// `duration_ms: 8`, `tool_response.isAsync: true` and `status:
/// "async_launched"` — the call returns at LAUNCH — while the agent it
/// launched ran 6–16 seconds more (`tests/fixtures/hooks/0009`). Giving
/// the seat back on `PostToolUse` therefore gave it back at launch, and
/// `max_active_agents_per_session` stopped binding: every agent past the
/// cap was admitted, its predecessors' seats already free.
///
/// `SubagentStop` is the agent's real end, but it names the agent
/// (`agent_id`), never the tool call, so the dispatch id — derived from
/// `session_id` and `tool_use_id` — cannot be recomputed from it. The
/// join exists in one payload only: the `PostToolUse`, whose
/// `tool_response.agentId` names the agent this `tool_use_id` launched.
/// So the `Post` binds the dispatch to that agent, and the `Stop` asks
/// the coordinator to settle whatever is bound to its agent. The order
/// of the two does not matter — a synchronous spawn's `SubagentStop`
/// arrives BEFORE its `PostToolUse` (`tests/fixtures/hooks-concurrent`),
/// and the coordinator remembers a stop that found nothing bound, so
/// the late bind ends the dispatch at once.
///
/// An agent whose `SubagentStop` never arrives holds its seat until its
/// lease lapses (`binding_lease_secs`): the cap over-counts for that
/// long, and never under-counts. A `PostToolUse` that names no agent (a
/// `PostToolUseFailure`, which launched nothing, or a payload from a
/// harness that does not carry `agentId`) binds nothing, and that seat
/// too goes back when the unbound dispatch lapses.
///
/// Settled with `None` rather than a figure: a hook payload carries no
/// usage, and `None` books the reservation as a lower bound and marks the
/// run uncertain, which is what not knowing looks like when it is
/// recorded honestly. Zero would be a claim that the agent was free.
/// Both ends together let the coordinator forget the entry.
fn follow_agent_lifecycle(event: &HookEvent, gate: &dyn Gate) {
    match event {
        HookEvent::AgentToolCall(call) => match &call.phase {
            ToolCallPhase::Post {
                launched_agent: Some(agent_id),
            } => {
                let dispatch_id = ids::derive_dispatch_id(&call.session_id, &call.tool_use_id);
                // Best effort, and `UnknownDispatch` much of the time: an
                // agent this relais never admitted — spawned before the
                // hook was wired, or while the coordinator was down —
                // returns its call like any other, and there is no seat to
                // bind. A hook cannot fail on bookkeeping any more than on
                // the decision itself, so the result changes nothing the
                // session is told. `Known`: the payload itself names both
                // the tool call and the agent, so the join is observed.
                let _ = gate.bind_agent_lease(
                    dispatch_id.as_str(),
                    agent_id.as_str(),
                    &Provenance::Known,
                );
            }
            // Nothing launched, or nothing named: no agent to bind the
            // seat to. Not released either — `Post` is not the agent's
            // end — so it waits for its lease to lapse.
            ToolCallPhase::Post {
                launched_agent: None,
            }
            | ToolCallPhase::PostFailure
            | ToolCallPhase::Pre => {}
        },
        HookEvent::SubagentStop(stop) => {
            // Best effort for the same reason as the bind above, and
            // `NothingBound` is an ordinary answer rather than a failure:
            // an agent relais never admitted ends like any other.
            let _ = gate.settle_by_agent(stop.session_id.as_str(), stop.agent_id.as_str(), None);
        }
        HookEvent::SessionStart(_)
        | HookEvent::SessionEnd(_)
        | HookEvent::SubagentStart(_)
        | HookEvent::NotOurs => {}
    }
}

/// Ask the coordinator about one spawn, and — when it queues — wait for a
/// seat. Only a `PreToolUse` on the Agent tool is ever asked about: every
/// other event is answered `None` without a call, because
/// [`decide`](super::decide::decide) never refuses one, and the agent's
/// later life is followed by [`follow_agent_lifecycle`] rather than by
/// asking anything.
///
/// Returns the coordinator's final answer alongside how long this firing
/// waited for it: the sleeping happens entirely in [`wait_for_a_seat`],
/// never in `decide`.
fn ask_coordinator(
    event: &HookEvent,
    settings: &HookAdmissionSettings,
    gate: &dyn Gate,
) -> (CoordinatorAnswer, Duration) {
    let HookEvent::AgentToolCall(call) = event else {
        return (None, Duration::ZERO);
    };
    if call.phase != ToolCallPhase::Pre {
        return (None, Duration::ZERO);
    }
    let dispatch_id = ids::derive_dispatch_id(&call.session_id, &call.tool_use_id);
    let run_id = ids::derive_run_id(&call.session_id);
    let request = DispatchRequest {
        dispatch_id: dispatch_id.as_str().to_string(),
        run_id: run_id.as_str().to_string(),
        session_id: call.session_id.as_str().to_string(),
        // `parent_dispatch` is never set here — this hook has no dispatch
        // id for its caller, only an agent id — but that is not the same
        // claim `hook::event`'s module doc used to make about a single
        // payload joining nothing at all. As of Claude Code 2.1.283 a
        // nested spawn's own `PreToolUse` carries `call.caller_agent_id`
        // (`tests/fixtures/hooks/README.md`), and the coordinator resolves
        // that agent id into the dispatch it is bound to and uses THAT as
        // this request's effective parent
        // (`admission::AdmissionState::resolve_caller`) — so depth is
        // enforced on this path whenever the caller resolves, through the
        // one `effective_depth` calculation every other dispatch already
        // goes through.
        parent_dispatch: None,
        depth: 0,
        resource: ResourceClass::ModelWork,
        reserve_micros: settings.dispatch_reserve_micros.to_micros(),
        source: DispatchSource::HookAdmitted,
        caller_agent_id: call
            .caller_agent_id
            .as_ref()
            .map(|id| id.as_str().to_string()),
    };
    // Any failure to reach the coordinator — no daemon, a stale
    // socket, a protocol mismatch — is exactly `CoordinatorAnswer`'s
    // `None`: "could not be reached at all", handled by
    // `HookAdmissionSettings::on_coordinator_unreachable` inside
    // `decide` rather than here.
    let Some(first) = gate.admit(&request).ok() else {
        return (None, Duration::ZERO);
    };
    let Decision::Refused {
        code: Refusal::UnknownRun,
        ..
    } = &first
    else {
        return wait_for_a_seat(first, settings, gate, &request);
    };
    // The first spawn of a session finds no run on record — nothing
    // registers one before it — so this fills in the record its own
    // question needs, deriving the run id from the session in hand
    // (never one a caller handed it, which is what keeps this from
    // laundering a run relais never derived itself) and retries once.
    // No limits or budget are named here: the caps in force are the
    // machine's own `[concurrency]` limits, already applied by the
    // coordinator to any run it holds no narrower registration for.
    let registration = RunRegistration {
        run_id: run_id.as_str().to_string(),
        session_id: call.session_id.as_str().to_string(),
        budget_micros: None,
        max_agents: None,
        max_depth: None,
    };
    // A coordinator that has gone away between the first answer and
    // this call is UNREACHABLE, not a coordinator that has no record of
    // the run: reporting the stored `UnknownRun` here would tell the
    // session to restart itself over what is actually an outage, and
    // `on_coordinator_unreachable` — silence or refusal, the machine's
    // choice — would never get to decide. `None` is how this function
    // says "could not be reached at all".
    if gate.register_run(&registration).is_err() {
        return (None, Duration::ZERO);
    }
    // At most one retry per firing: whatever this second call answers
    // — admitted, still unknown, or something else entirely — is what
    // the hook waits on (or is told at once). No third attempt.
    let Some(retried) = gate.admit(&request).ok() else {
        return (None, Duration::ZERO);
    };
    wait_for_a_seat(retried, settings, gate, &request)
}

/// Poll for a seat behind a `Queued` answer, re-asking with the same
/// dispatch ID at `runner::ADMISSION_POLL` cadence — the protocol
/// `admission::AdmissionState::request` already speaks for `relais run`'s
/// own managed dispatch, reused here rather than invented a second time —
/// for up to `settings.queue_behaviour()`'s bound.
///
/// Measured on Claude Code 2.1.282 (SPEC §23): a `PreToolUse` hook holds
/// its tool call open for as long as it runs, so this sleeping is exactly
/// what a hook can do that `decide` cannot decide to do — it is not pure,
/// it touches the clock and the socket, and it lives here rather than
/// there for that reason. `RefuseImmediately` and anything that is not
/// `Queued` return at once with no wait at all.
///
/// Gives up at its own deadline, withdrawing the request so a caller that
/// reaches this line does not strand its own seat — the same seat
/// `admission::UNCLAIMED_GRACE` reaps regardless, for the caller that
/// cannot reach this line at all (killed mid-wait).
fn wait_for_a_seat(
    mut decision: Decision,
    settings: &HookAdmissionSettings,
    gate: &dyn Gate,
    request: &DispatchRequest,
) -> (CoordinatorAnswer, Duration) {
    if !matches!(decision, Decision::Queued { .. }) {
        return (Some(decision), Duration::ZERO);
    }
    let max_wait = match settings.queue_behaviour() {
        QueueBehaviour::RefuseImmediately => return (Some(decision), Duration::ZERO),
        QueueBehaviour::WaitUpTo(max) => max,
    };
    let started = Instant::now();
    let deadline = started + max_wait;
    while matches!(decision, Decision::Queued { .. }) && Instant::now() < deadline {
        std::thread::sleep(ADMISSION_POLL);
        let Some(next) = gate.admit(request).ok() else {
            return (None, started.elapsed());
        };
        decision = next;
    }
    if matches!(decision, Decision::Queued { .. }) {
        // Deliberately duplicated: `handle_inner`'s refusal path also
        // withdraws, and today that covers this case, because `decide`
        // renders every `Queued` as a refusal. Delete this and no test
        // fails — the one that looks like it would, passes on the other
        // withdrawal. It is kept because the coupling is the fragile
        // part: the day `Queued` stops meaning refuse, or a wait gives
        // up into some other answer, this entry has to leave the queue
        // regardless, and the seat it holds is not free until it does.
        let _ = gate.withdraw(request.dispatch_id.as_str());
    }
    (Some(decision), started.elapsed())
}

/// What this firing's wait came of, distinguished so backpressure and
/// stalling do not collapse into one number in the journal — the only
/// record either of them leaves.
fn wait_outcome(handled: &Handled) -> &'static str {
    if handled.waited.is_zero() {
        return "immediate";
    }
    match &handled.answer {
        HookAnswer::Silent => "admitted_after_waiting",
        HookAnswer::Refuse { .. } => "refused_after_waiting",
    }
}

/// `Refusal`'s availability, spelled the way a journal reader — or
/// `relais doctor` — groups by it: `Some` on a refusal, `None` on silence,
/// never a third string standing in for "not applicable".
fn availability_label(availability: &super::decide::Availability) -> &'static str {
    match availability {
        super::decide::Availability::Decided => "decided",
        super::decide::Availability::Unknown => "unknown",
    }
}

/// What one firing is journalled as: what arrived, what the
/// coordinator answered (when it was asked), how long it waited and what
/// came of that, and what was decided. Pure — it builds the record,
/// [`append_journal`] writes it.
pub fn journal_entry(payload: &[u8], handled: &Handled) -> serde_json::Value {
    // The raw payload, not a re-serialization of the typed event: a
    // payload this crate does not model (a future harness field, or
    // something that failed to parse at all) is still what arrived,
    // and `event` below already records what relais made of it.
    let payload_value = serde_json::from_slice::<serde_json::Value>(payload).unwrap_or_else(|_| {
        serde_json::Value::String(String::from_utf8_lossy(payload).into_owned())
    });
    let (decision, reason, rule, availability) = match &handled.answer {
        HookAnswer::Silent => ("silent", None, None, None),
        HookAnswer::Refuse { refusal } => (
            "refuse",
            Some(refusal.sentence()),
            Some(refusal.rule.label()),
            Some(availability_label(&refusal.rule.availability())),
        ),
    };
    serde_json::json!({
        "recorded_at": chrono::Utc::now().to_rfc3339(),
        "payload": payload_value,
        // `Debug`, deliberately, and only as a human-readable gloss: the
        // structured record of what arrived is `payload` beside it, which
        // is the harness's own JSON unaltered. Nothing should parse these
        // two strings — `Debug` output is not a format anything may
        // depend on — and they exist because a person reading a line
        // wants to see how relais UNDERSTOOD the payload, not only what
        // it received. If a reader ever needs to query them, they should
        // be given a serialized shape rather than have this one parsed.
        "event_debug": format!("{:?}", handled.event),
        "coordinator_debug": handled
            .coordinator
            .as_ref()
            .map(|decision| format!("{decision:?}")),
        "decision": decision,
        "reason": reason,
        "rule": rule,
        "availability": availability,
        "waited_ms": handled.waited.as_millis() as u64,
        "outcome": wait_outcome(handled),
    })
}

/// Append one journal entry as a single JSON line, creating the file
/// owner-only if it does not exist yet. A payload names a person's
/// transcript path and working directory (SPEC §23), so the file this
/// writes to is never created with group or other access.
///
/// The line is rendered into one buffer, newline included, and handed to
/// a single `write_all`. `O_APPEND` makes each individual write atomic in
/// where it lands, never a group of them, and `writeln!` on a `File` goes
/// through `Write::write_fmt`, which issues one write per formatted
/// fragment — `Display for serde_json::Value` streams a record as braces,
/// keys, colons and values, so a single line left dozens of syscalls
/// behind, not two. Two firings at the same instant interleaved those
/// fragments and left lines that were not JSON (issue #78). Folding the
/// newline into the `writeln!` would have fixed nothing; what matters is
/// that the whole record is one write.
pub fn append_journal(path: &Path, entry: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = open_owner_only(path)?;
    let mut line = entry.to_string();
    line.push('\n');
    file.write_all(line.as_bytes())
}

#[cfg(unix)]
fn open_owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    // `.mode` is masked by the process umask at creation, and this file
    // is appended to across separate hook invocations, so a permissive
    // umask could leave an EXISTING file wider than 0600 from its very
    // first line. Set explicitly, matching `ipc::Listener::bind`.
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = file.metadata()?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions)?;
    Ok(file)
}

/// On Windows the journal inherits the state directory's ACL and this
/// function narrows nothing — the name promises more than the platform
/// arm delivers, and it is written here rather than left for a reader to
/// infer from an absent call.
///
/// Restricting it properly needs a DACL built through `windows-sys`,
/// which only `procs` is allowed to name (`scripts/check-module-cycles.py`),
/// so doing it here would either breach that boundary or move
/// journalling into a module that has no business owning it. The
/// payloads carry a transcript path and a working directory, so this is
/// a real gap on that target, not a cosmetic one: a Windows user's
/// journal is as readable as the directory relais put it in.
#[cfg(windows)]
fn open_owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{LocalGate, RunRegistration};
    use crate::ids::{SessionId, ToolUseId};
    use crate::policy::{ConcurrencyLimits, CoordinatorUnreachableBehavior};

    // Zero: every existing test here predates the wait and expects an
    // immediate refusal at a cap. The wait itself gets its own settings
    // and its own tests, below.
    fn settings() -> HookAdmissionSettings {
        HookAdmissionSettings {
            binding_lease_secs: 120,
            dispatch_reserve_micros: crate::money::MicroUsd::ZERO,
            on_coordinator_unreachable: CoordinatorUnreachableBehavior::CarryOn,
            queue_wait_secs: 0,
        }
    }

    /// A short, real wait — long enough for a background thread to free a
    /// seat mid-poll, short enough that a test which exhausts it stays
    /// fast.
    fn waiting_settings(queue_wait_secs: u64) -> HookAdmissionSettings {
        HookAdmissionSettings {
            queue_wait_secs,
            ..settings()
        }
    }

    fn spawn_payload(session: &str, tool_use: &str) -> Vec<u8> {
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": session,
            "tool_name": "Agent",
            "tool_use_id": tool_use,
        })
        .to_string()
        .into_bytes()
    }

    /// A spawn made FROM INSIDE a subagent — the shape a nested `Agent`
    /// `PreToolUse` carries on Claude Code 2.1.283, `agent_id` naming the
    /// caller rather than being absent (`0004-PreToolUse.json`,
    /// `tests/fixtures/hooks/README.md`).
    fn nested_spawn_payload(session: &str, tool_use: &str, caller_agent: &str) -> Vec<u8> {
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": session,
            "tool_name": "Agent",
            "tool_use_id": tool_use,
            "agent_id": caller_agent,
        })
        .to_string()
        .into_bytes()
    }

    /// The spawn's own tool call returning, the way a real async launch
    /// does (`tests/fixtures/hooks/0009-PostToolUse.json`): at once, with
    /// the agent still running, naming the agent it launched. Same
    /// `tool_use_id` as the spawn, so the same derived dispatch id.
    fn launched_payload(session: &str, tool_use: &str, agent: &str) -> Vec<u8> {
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": session,
            "tool_name": "Agent",
            "tool_use_id": tool_use,
            "duration_ms": 8,
            "tool_response": {
                "isAsync": true,
                "status": "async_launched",
                "agentId": agent,
            },
        })
        .to_string()
        .into_bytes()
    }

    /// The agent's real end (`tests/fixtures/hooks/0010-SubagentStop.json`):
    /// it names the agent, and no tool call.
    fn stop_payload(session: &str, agent: &str) -> Vec<u8> {
        serde_json::json!({
            "hook_event_name": "SubagentStop",
            "session_id": session,
            "agent_id": agent,
            "agent_type": "general-purpose",
            "background_tasks": [],
        })
        .to_string()
        .into_bytes()
    }

    /// The first spawn of a session the coordinator has never heard of
    /// is admitted on its merits rather than refused for having no
    /// record: the hook derives and registers that session's own run
    /// and retries the admission once, silently, against a real
    /// coordinator that was never told about the session beforehand.
    #[test]
    fn a_spawn_in_an_unheard_of_session_registers_its_run_and_is_admitted_silently() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        let payload = spawn_payload("session-unknown", "tool-1");
        let handled = handle(&payload, &settings(), &gate);
        assert_eq!(handled.answer, HookAnswer::Silent);
        assert!(matches!(
            handled.coordinator,
            Some(crate::admission::Decision::Granted)
        ));
    }

    /// A coordinator that keeps answering `UnknownRun` — even after a
    /// registration — is retried at most once: exactly one
    /// registration, and the SECOND refusal is what reaches stdout, not
    /// a third attempt.
    #[test]
    fn the_retry_happens_at_most_once_per_firing() {
        struct AlwaysUnknownRun {
            inner: LocalGate,
            registrations: std::sync::atomic::AtomicUsize,
        }
        impl Gate for AlwaysUnknownRun {
            fn register_run(
                &self,
                registration: &RunRegistration,
            ) -> Result<(), crate::admission::GateError> {
                self.registrations
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.inner.register_run(registration)
            }
            fn admit(
                &self,
                request: &DispatchRequest,
            ) -> Result<crate::admission::Decision, crate::admission::GateError> {
                // Passed through so the inner state still sees the
                // request — the point of this stub is the ANSWER, not
                // starving the state underneath it — and its own answer
                // is dropped on purpose, because this gate exists to
                // answer `UnknownRun` however often it is asked.
                let _ = self.inner.admit(request);
                Ok(crate::admission::Decision::Refused {
                    code: Refusal::UnknownRun,
                    detail: format!("run {} is not registered", request.run_id),
                })
            }
            fn bind(
                &self,
                _: &str,
                _: Option<&str>,
                _: Option<u32>,
            ) -> Result<crate::admission::BindOutcome, crate::admission::GateError> {
                unreachable!()
            }
            fn heartbeat(
                &self,
                _: &str,
            ) -> Result<crate::admission::HeartbeatStatus, crate::admission::GateError>
            {
                unreachable!()
            }
            fn mark_waiting(
                &self,
                _: &str,
            ) -> Result<crate::admission::WaitOutcome, crate::admission::GateError> {
                unreachable!()
            }
            fn resume(
                &self,
                _: &str,
            ) -> Result<crate::admission::ResumeOutcome, crate::admission::GateError> {
                unreachable!()
            }
            fn release(
                &self,
                _: &str,
            ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError>
            {
                unreachable!()
            }
            fn settle(
                &self,
                _: &str,
                _: Option<i64>,
            ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError>
            {
                unreachable!()
            }
            fn withdraw(
                &self,
                dispatch_id: &str,
            ) -> Result<crate::admission::WithdrawOutcome, crate::admission::GateError>
            {
                self.inner.withdraw(dispatch_id)
            }
            fn enforcement(&self) -> crate::admission::Enforcement {
                crate::admission::Enforcement::InProcess
            }
        }
        let gate = AlwaysUnknownRun {
            inner: LocalGate::new(ConcurrencyLimits::default()),
            registrations: std::sync::atomic::AtomicUsize::new(0),
        };
        let payload = spawn_payload("session-stubborn", "tool-1");
        let handled = handle(&payload, &settings(), &gate);
        assert_eq!(
            gate.registrations.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "must register at most once per firing"
        );
        let reason = match &handled.answer {
            HookAnswer::Refuse { refusal } => refusal.sentence(),
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert!(reason.contains("is not registered"), "{reason}");
        let stdout = handled.answer.stdout_payload().expect("refusal on stdout");
        assert!(stdout.contains("is not registered"));
    }

    /// Which call an outage lands on. Not a flag: the two cases are
    /// different moments in the same sequence, and the answer the hook
    /// must give is the same for both — which is the point.
    #[derive(Clone, Copy)]
    enum OutageAt {
        /// The very first `admit`: nothing is known yet.
        FirstAdmit,
        /// `admit` answered `UnknownRun`, and the coordinator went away
        /// before the registration that answer prompts.
        Registration,
    }

    struct Unreachable {
        at: OutageAt,
        registrations: std::sync::atomic::AtomicUsize,
    }

    impl Unreachable {
        fn unavailable<T>(operation: &'static str) -> Result<T, crate::admission::GateError> {
            Err(crate::admission::GateError::Unavailable {
                operation,
                socket: "test".into(),
                cause: Box::new(std::io::Error::other("no daemon")),
            })
        }
    }

    impl Gate for Unreachable {
        fn register_run(&self, _: &RunRegistration) -> Result<(), crate::admission::GateError> {
            self.registrations
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match self.at {
                OutageAt::FirstAdmit => Ok(()),
                OutageAt::Registration => Self::unavailable("register_run"),
            }
        }
        fn admit(
            &self,
            request: &DispatchRequest,
        ) -> Result<crate::admission::Decision, crate::admission::GateError> {
            match self.at {
                OutageAt::FirstAdmit => Self::unavailable("admit"),
                OutageAt::Registration => Ok(crate::admission::Decision::Refused {
                    code: Refusal::UnknownRun,
                    detail: format!("run {} is not registered", request.run_id),
                }),
            }
        }
        fn bind(
            &self,
            _: &str,
            _: Option<&str>,
            _: Option<u32>,
        ) -> Result<crate::admission::BindOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn heartbeat(
            &self,
            _: &str,
        ) -> Result<crate::admission::HeartbeatStatus, crate::admission::GateError> {
            unreachable!()
        }
        fn mark_waiting(
            &self,
            _: &str,
        ) -> Result<crate::admission::WaitOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn resume(
            &self,
            _: &str,
        ) -> Result<crate::admission::ResumeOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn release(
            &self,
            _: &str,
        ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn settle(
            &self,
            _: &str,
            _: Option<i64>,
        ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError> {
            unreachable!()
        }
        /// Answered rather than `unreachable!()`, and the difference
        /// matters: the refusal path calls `withdraw`, so a panicking
        /// stub here would be caught by `handle`'s `catch_unwind` and
        /// turned into `Silent` — which is what these tests assert, so
        /// they would pass for a reason that has nothing to do with the
        /// behaviour under test. A down coordinator cannot take a
        /// withdrawal either, so this is also what really happens.
        fn withdraw(
            &self,
            _: &str,
        ) -> Result<crate::admission::WithdrawOutcome, crate::admission::GateError> {
            Self::unavailable("withdraw")
        }
        fn enforcement(&self) -> crate::admission::Enforcement {
            crate::admission::Enforcement::InProcess
        }
    }
    /// A coordinator that cannot be reached at all is never registered
    /// against: `admit` returning an outage keeps today's behaviour
    /// (silent under `carry_on`) and attempts no registration.
    #[test]
    fn an_unreachable_coordinator_is_never_registered_against() {
        let gate = Unreachable {
            at: OutageAt::FirstAdmit,
            registrations: std::sync::atomic::AtomicUsize::new(0),
        };
        let handled = handle(&spawn_payload("session-down", "tool-1"), &settings(), &gate);
        assert_eq!(handled.answer, HookAnswer::Silent);
        assert_eq!(handled.coordinator, None);
        assert_eq!(
            gate.registrations.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "an unreachable coordinator must not be registered against"
        );
    }

    /// An outage that arrives BETWEEN the first answer and the
    /// registration it prompts is still an outage, and is answered like
    /// one: silent under `carry_on`. Folding it into the stored
    /// `UnknownRun` instead would tell the session to restart itself over
    /// a coordinator that is merely down, and would take the choice
    /// `on_coordinator_unreachable` exists to make away from the machine
    /// that configured it.
    #[test]
    fn a_coordinator_that_dies_during_registration_is_an_outage_not_an_unknown_run() {
        let gate = Unreachable {
            at: OutageAt::Registration,
            registrations: std::sync::atomic::AtomicUsize::new(0),
        };
        let handled = handle(
            &spawn_payload("session-dying", "tool-1"),
            &settings(),
            &gate,
        );
        assert_eq!(
            handled.answer,
            HookAnswer::Silent,
            "carry_on answers an outage with silence, whenever in the sequence it lands"
        );
        assert_eq!(
            handled.coordinator, None,
            "and records that the coordinator could not be reached, not a refusal it gave"
        );
        assert_eq!(
            gate.registrations.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the registration was attempted exactly once before the outage was seen"
        );
    }

    /// Registering cannot resurrect a cancelled run: the hook only ever
    /// registers the run id it derives from the session in hand, so a
    /// run cancelled after registration stays refused as cancelled
    /// rather than coming back to life on the next spawn's retry.
    #[test]
    fn a_cancelled_run_stays_refused_after_a_registration() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        let session = SessionId::new("session-cancelled");
        let run_id = ids::derive_run_id(&session);
        gate.register_run(&RunRegistration {
            run_id: run_id.as_str().to_string(),
            session_id: session.as_str().to_string(),
            budget_micros: None,
            max_agents: None,
            max_depth: None,
        })
        .expect("register");
        gate.cancel_run(run_id.as_str());
        let payload = spawn_payload(session.as_str(), "tool-1");
        let handled = handle(&payload, &settings(), &gate);
        match &handled.answer {
            HookAnswer::Refuse { refusal } => {
                assert!(
                    refusal.sentence().contains("cancelled"),
                    "{}",
                    refusal.sentence()
                )
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(matches!(
            handled.coordinator,
            Some(crate::admission::Decision::Refused {
                code: Refusal::RunCancelled,
                ..
            })
        ));
    }

    /// The other side of that answer, and the reason the test above is
    /// named for a registration rather than for a cancellation that
    /// holds: a cancelled run with nothing outstanding is REAPED at the
    /// next reconcile, and afterwards the derived run id is simply
    /// unknown — indistinguishable, to a hook, from a session it has
    /// never seen, which is exactly the state that makes it register the
    /// run and be admitted. So `relais cancel` on a session's own run
    /// stops its spawns for as long as the coordinator remembers it and
    /// no longer; README and SPEC §23 say so in those terms.
    ///
    /// Driven against `AdmissionState` directly because the reap belongs
    /// to reconciliation, which no gate exposes — but the fact is about
    /// the hook, so it is recorded here beside the answer it undoes.
    #[test]
    fn a_reaped_cancellation_leaves_the_run_unknown_again() {
        use crate::admission::AdmissionState;
        use std::time::{Duration, Instant};

        let session = SessionId::new("session-reaped");
        let run_id = ids::derive_run_id(&session);
        let start = Instant::now();
        let mut state = AdmissionState::new(ConcurrencyLimits::default());
        state.register_run(
            &RunRegistration {
                run_id: run_id.as_str().to_string(),
                session_id: session.as_str().to_string(),
                budget_micros: None,
                max_agents: None,
                max_depth: None,
            },
            start,
        );
        let _signals = state.cancel_run(run_id.as_str(), start);

        let request = |tool_use: &str| DispatchRequest {
            dispatch_id: ids::derive_dispatch_id(&session, &ToolUseId::new(tool_use))
                .as_str()
                .to_string(),
            run_id: run_id.as_str().to_string(),
            session_id: session.as_str().to_string(),
            parent_dispatch: None,
            depth: 0,
            resource: ResourceClass::ModelWork,
            reserve_micros: 0,
            source: DispatchSource::HookAdmitted,
            caller_agent_id: None,
        };

        assert!(
            matches!(
                state.request(&request("tool-1"), start),
                Decision::Refused {
                    code: Refusal::RunCancelled,
                    ..
                }
            ),
            "while the coordinator remembers the cancellation, it refuses"
        );

        // One reconcile later the terminal run is gone, and with it every
        // trace of the cancellation.
        state.reconcile(start + Duration::from_secs(1), &|_| true);
        assert!(
            matches!(
                state.request(&request("tool-2"), start + Duration::from_secs(1)),
                Decision::Refused {
                    code: Refusal::UnknownRun,
                    ..
                }
            ),
            "after the reap the run is unknown, which is what the hook then registers afresh"
        );
    }

    /// A machine whose per-session agent cap is small enough to bind:
    /// the first N spawns of one session are silent, and the (N+1)th is
    /// refused with a message that names the limit and a remedy — never
    /// `UnknownRun`, which would prove nothing about the cap itself.
    #[test]
    fn a_cap_is_observed_refusing_a_spawn_through_the_command() {
        let limits = ConcurrencyLimits {
            max_active_agents_per_session: Some(2),
            ..ConcurrencyLimits::default()
        };
        let gate = LocalGate::new(limits);
        let session = "session-capped";
        for tool_use in ["tool-1", "tool-2"] {
            let payload = spawn_payload(session, tool_use);
            let handled = handle(&payload, &settings(), &gate);
            assert_eq!(
                handled.answer,
                HookAnswer::Silent,
                "spawn {tool_use} should be under the cap"
            );
        }
        let payload = spawn_payload(session, "tool-3");
        let handled = handle(&payload, &settings(), &gate);
        let reason = match &handled.answer {
            HookAnswer::Refuse { refusal } => refusal.sentence(),
            other => panic!("expected the capped spawn to be refused, got {other:?}"),
        };
        assert!(
            !reason.contains("is not registered"),
            "the cap refusal must not be UnknownRun: {reason}"
        );
        assert!(
            reason.contains("limit") || reason.contains("queued"),
            "the refusal must name the cap: {reason}"
        );
    }

    /// What makes that cap a cap on RUNNING agents: a seat is held from
    /// the spawn until the AGENT stops, and the tool call returning in
    /// between frees nothing. A real async launch returns its
    /// `PostToolUse` ~100ms after the spawn while the agent runs on, so
    /// the assertion that matters here is the refusal AFTER both
    /// `PostToolUse`s: release-on-`PostToolUse` passes every other line
    /// of this test and fails exactly that one — the cap would have
    /// stopped binding the moment each agent launched.
    #[test]
    fn a_seat_is_held_until_its_agent_stops_not_until_its_call_returns() {
        let limits = ConcurrencyLimits {
            max_active_agents_per_session: Some(2),
            ..ConcurrencyLimits::default()
        };
        let gate = LocalGate::new(limits);
        let session = "session-freed";
        for (tool_use, agent) in [("tool-1", "agent-1"), ("tool-2", "agent-2")] {
            assert_eq!(
                handle(&spawn_payload(session, tool_use), &settings(), &gate).answer,
                HookAnswer::Silent,
                "spawn {tool_use} is under the cap"
            );
            // The call returns at launch, naming the agent it launched.
            assert_eq!(
                handle(
                    &launched_payload(session, tool_use, agent),
                    &settings(),
                    &gate
                )
                .answer,
                HookAnswer::Silent,
                "the return of a call is never refused"
            );
        }
        assert!(
            matches!(
                handle(&spawn_payload(session, "tool-3"), &settings(), &gate).answer,
                HookAnswer::Refuse { .. }
            ),
            "both calls have returned but both agents still run: the third spawn is over the cap"
        );

        // agent-1 ends. Its `SubagentStop` names no tool call; the seat is
        // found through the binding its `PostToolUse` made.
        let stopped = handle(&stop_payload(session, "agent-1"), &settings(), &gate);
        assert_eq!(
            stopped.answer,
            HookAnswer::Silent,
            "an agent's end is never refused"
        );

        assert_eq!(
            handle(&spawn_payload(session, "tool-4"), &settings(), &gate).answer,
            HookAnswer::Silent,
            "with the stopped agent's seat given back, the next spawn is admitted"
        );
    }

    /// A synchronous spawn's `SubagentStop` arrives BEFORE its own
    /// `PostToolUse` (`tests/fixtures/hooks-concurrent`), so when the
    /// stop fires nothing is bound to its agent yet. The seat is still
    /// given back — by the bind that follows — rather than held until the
    /// lease lapses.
    #[test]
    fn an_agent_that_stops_before_its_call_returns_still_gives_its_seat_back() {
        let limits = ConcurrencyLimits {
            max_active_agents_per_session: Some(1),
            ..ConcurrencyLimits::default()
        };
        let gate = LocalGate::new(limits);
        let session = "session-sync";
        assert_eq!(
            handle(&spawn_payload(session, "tool-1"), &settings(), &gate).answer,
            HookAnswer::Silent
        );
        handle(&stop_payload(session, "agent-1"), &settings(), &gate);
        handle(
            &launched_payload(session, "tool-1", "agent-1"),
            &settings(),
            &gate,
        );
        assert_eq!(
            handle(&spawn_payload(session, "tool-2"), &settings(), &gate).answer,
            HookAnswer::Silent,
            "the agent is over, whichever of its two ends arrived first"
        );
    }

    /// The wiring this module owns: a `PreToolUse`'s `agent_id` — absent
    /// on a top-level spawn, present on one made from inside a subagent
    /// (`hook::event::AgentToolCall::caller_agent_id`) — reaches the
    /// `DispatchRequest` the coordinator resolves against, rather than
    /// being dropped on the way as `parent_dispatch: None` used to mean
    /// "not knowable here" for every hook-admitted request. What the
    /// coordinator DOES with a resolved caller is `admission::mod`'s own
    /// tests (`a_nested_spawn_naming_a_bound_caller_...`); this one is
    /// about the one field this module is responsible for setting.
    #[test]
    fn a_nested_spawns_caller_agent_id_reaches_the_dispatch_request() {
        struct CapturingGate {
            inner: LocalGate,
            last_caller_agent_id: std::sync::Mutex<Option<Option<String>>>,
        }
        impl Gate for CapturingGate {
            fn register_run(
                &self,
                registration: &RunRegistration,
            ) -> Result<(), crate::admission::GateError> {
                self.inner.register_run(registration)
            }
            fn admit(
                &self,
                request: &DispatchRequest,
            ) -> Result<crate::admission::Decision, crate::admission::GateError> {
                *self.last_caller_agent_id.lock().unwrap() = Some(request.caller_agent_id.clone());
                self.inner.admit(request)
            }
            fn bind(
                &self,
                dispatch_id: &str,
                agent_id: Option<&str>,
                pid: Option<u32>,
            ) -> Result<crate::admission::BindOutcome, crate::admission::GateError> {
                self.inner.bind(dispatch_id, agent_id, pid)
            }
            fn heartbeat(
                &self,
                dispatch_id: &str,
            ) -> Result<crate::admission::HeartbeatStatus, crate::admission::GateError>
            {
                self.inner.heartbeat(dispatch_id)
            }
            fn mark_waiting(
                &self,
                dispatch_id: &str,
            ) -> Result<crate::admission::WaitOutcome, crate::admission::GateError> {
                self.inner.mark_waiting(dispatch_id)
            }
            fn resume(
                &self,
                dispatch_id: &str,
            ) -> Result<crate::admission::ResumeOutcome, crate::admission::GateError> {
                self.inner.resume(dispatch_id)
            }
            fn release(
                &self,
                dispatch_id: &str,
            ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError>
            {
                self.inner.release(dispatch_id)
            }
            fn settle(
                &self,
                dispatch_id: &str,
                spent_micros: Option<i64>,
            ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError>
            {
                self.inner.settle(dispatch_id, spent_micros)
            }
            fn withdraw(
                &self,
                dispatch_id: &str,
            ) -> Result<crate::admission::WithdrawOutcome, crate::admission::GateError>
            {
                self.inner.withdraw(dispatch_id)
            }
            fn enforcement(&self) -> crate::admission::Enforcement {
                self.inner.enforcement()
            }
        }
        let gate = CapturingGate {
            inner: LocalGate::new(ConcurrencyLimits::default()),
            last_caller_agent_id: std::sync::Mutex::new(None),
        };
        handle(&spawn_payload("session-a", "tool-top"), &settings(), &gate);
        assert_eq!(
            *gate.last_caller_agent_id.lock().unwrap(),
            Some(None),
            "a top-level spawn's payload carries no agent_id and reports no caller"
        );

        handle(
            &nested_spawn_payload("session-a", "tool-nested", "caller-agent-1"),
            &settings(),
            &gate,
        );
        assert_eq!(
            *gate.last_caller_agent_id.lock().unwrap(),
            Some(Some("caller-agent-1".to_string())),
            "a nested spawn's payload names its caller, and this module forwards it"
        );
    }

    /// A `PostToolUseFailure` launched nothing and names no agent: it
    /// binds nothing and releases nothing, so the seat stays held (until
    /// the unbound dispatch lapses) rather than the cap under-counting.
    #[test]
    fn a_post_tool_use_failure_gives_nothing_back() {
        let limits = ConcurrencyLimits {
            max_active_agents_per_session: Some(1),
            ..ConcurrencyLimits::default()
        };
        let gate = LocalGate::new(limits);
        let session = "session-failed";
        assert_eq!(
            handle(&spawn_payload(session, "tool-1"), &settings(), &gate).answer,
            HookAnswer::Silent
        );
        let failure = serde_json::json!({
            "hook_event_name": "PostToolUseFailure",
            "session_id": session,
            "tool_name": "Agent",
            "tool_use_id": "tool-1",
        })
        .to_string()
        .into_bytes();
        assert_eq!(
            handle(&failure, &settings(), &gate).answer,
            HookAnswer::Silent
        );
        assert!(matches!(
            handle(&spawn_payload(session, "tool-2"), &settings(), &gate).answer,
            HookAnswer::Refuse { .. }
        ));
    }

    /// A registered run with room grants, and `decide` renders that as
    /// silence: no stdout payload at all.
    #[test]
    fn a_registered_run_with_room_is_silent() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        let session = SessionId::new("session-granted");
        let run_id = ids::derive_run_id(&session);
        gate.register_run(&RunRegistration {
            run_id: run_id.as_str().to_string(),
            session_id: session.as_str().to_string(),
            budget_micros: None,
            max_agents: None,
            max_depth: None,
        })
        .expect("register");
        let payload = spawn_payload(session.as_str(), "tool-2");
        let handled = handle(&payload, &settings(), &gate);
        assert_eq!(handled.answer, HookAnswer::Silent);
        assert_eq!(handled.answer.stdout_payload(), None);
        assert!(matches!(
            handled.coordinator,
            Some(crate::admission::Decision::Granted)
        ));
    }

    /// The refusal path calls `withdraw` for the dispatch it asked
    /// about, so a spawn relais refuses is never left holding whatever
    /// its own request reserved — the property SPEC §23 exists to
    /// guard, proven through the command's own wiring rather than by
    /// inspecting admission state directly.
    #[test]
    fn a_refusal_withdraws_the_dispatch_it_asked_about() {
        struct CountingWithdraw {
            inner: LocalGate,
            withdrawals: std::sync::atomic::AtomicUsize,
        }
        impl Gate for CountingWithdraw {
            fn register_run(
                &self,
                registration: &RunRegistration,
            ) -> Result<(), crate::admission::GateError> {
                self.inner.register_run(registration)
            }
            fn admit(
                &self,
                request: &DispatchRequest,
            ) -> Result<crate::admission::Decision, crate::admission::GateError> {
                self.inner.admit(request)
            }
            fn bind(
                &self,
                dispatch_id: &str,
                agent_id: Option<&str>,
                pid: Option<u32>,
            ) -> Result<crate::admission::BindOutcome, crate::admission::GateError> {
                self.inner.bind(dispatch_id, agent_id, pid)
            }
            fn heartbeat(
                &self,
                dispatch_id: &str,
            ) -> Result<crate::admission::HeartbeatStatus, crate::admission::GateError>
            {
                self.inner.heartbeat(dispatch_id)
            }
            fn mark_waiting(
                &self,
                dispatch_id: &str,
            ) -> Result<crate::admission::WaitOutcome, crate::admission::GateError> {
                self.inner.mark_waiting(dispatch_id)
            }
            fn resume(
                &self,
                dispatch_id: &str,
            ) -> Result<crate::admission::ResumeOutcome, crate::admission::GateError> {
                self.inner.resume(dispatch_id)
            }
            fn release(
                &self,
                dispatch_id: &str,
            ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError>
            {
                self.inner.release(dispatch_id)
            }
            fn settle(
                &self,
                dispatch_id: &str,
                spent_micros: Option<i64>,
            ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError>
            {
                self.inner.settle(dispatch_id, spent_micros)
            }
            fn withdraw(
                &self,
                dispatch_id: &str,
            ) -> Result<crate::admission::WithdrawOutcome, crate::admission::GateError>
            {
                self.withdrawals
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.inner.withdraw(dispatch_id)
            }
            fn enforcement(&self) -> crate::admission::Enforcement {
                self.inner.enforcement()
            }
        }
        let gate = CountingWithdraw {
            inner: LocalGate::new(ConcurrencyLimits::default()),
            withdrawals: std::sync::atomic::AtomicUsize::new(0),
        };
        // Registered, then cancelled: the auto-registration this module
        // now does only fires on `UnknownRun`, so a cancelled run's
        // refusal is reached directly, on the first `admit`, and still
        // has to be withdrawn.
        let session = SessionId::new("session-refused");
        let run_id = ids::derive_run_id(&session);
        gate.register_run(&RunRegistration {
            run_id: run_id.as_str().to_string(),
            session_id: session.as_str().to_string(),
            budget_micros: None,
            max_agents: None,
            max_depth: None,
        })
        .expect("register");
        gate.inner.cancel_run(run_id.as_str());
        let payload = spawn_payload(session.as_str(), "tool-3");
        let handled = handle(&payload, &settings(), &gate);
        assert!(matches!(handled.answer, HookAnswer::Refuse { .. }));
        assert_eq!(
            gate.withdrawals.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the refusal path must withdraw the dispatch it asked about"
        );
    }

    /// A gate that fails the test if it is consulted at all — the way
    /// "this path must not reach the coordinator" is asserted, since a
    /// seat reserved on a path with no later release leaks silently.
    /// Shared: three copies differing only in this comment drift apart,
    /// and a `Gate` method added later would want editing three times.
    struct PanicsIfAsked;
    impl Gate for PanicsIfAsked {
        fn register_run(&self, _: &RunRegistration) -> Result<(), crate::admission::GateError> {
            unreachable!()
        }
        fn admit(
            &self,
            _: &DispatchRequest,
        ) -> Result<crate::admission::Decision, crate::admission::GateError> {
            panic!("must not be asked")
        }
        fn bind(
            &self,
            _: &str,
            _: Option<&str>,
            _: Option<u32>,
        ) -> Result<crate::admission::BindOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn heartbeat(
            &self,
            _: &str,
        ) -> Result<crate::admission::HeartbeatStatus, crate::admission::GateError> {
            unreachable!()
        }
        fn mark_waiting(
            &self,
            _: &str,
        ) -> Result<crate::admission::WaitOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn resume(
            &self,
            _: &str,
        ) -> Result<crate::admission::ResumeOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn release(
            &self,
            _: &str,
        ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn settle(
            &self,
            _: &str,
            _: Option<i64>,
        ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn withdraw(
            &self,
            _: &str,
        ) -> Result<crate::admission::WithdrawOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn bind_agent_lease(
            &self,
            _: &str,
            _: &str,
            _: &Provenance,
        ) -> Result<crate::admission::BindOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn settle_by_agent(
            &self,
            _: &str,
            _: &str,
            _: Option<i64>,
        ) -> Result<crate::admission::AgentSettleOutcome, crate::admission::GateError> {
            unreachable!()
        }
        fn enforcement(&self) -> crate::admission::Enforcement {
            crate::admission::Enforcement::InProcess
        }
    }

    /// A `PostToolUse` that names no launched agent, and a
    /// `PostToolUseFailure`, never reach the coordinator at all: there
    /// is nothing to admit (asking about a phase `decide` never refuses
    /// would reserve a seat nothing later gives back) and no agent to
    /// bind. The event is asserted too — `handle` turns a panic into
    /// `NotOurs`, so without it a gate that WAS asked would pass here.
    #[test]
    fn a_post_naming_no_agent_never_asks_the_coordinator() {
        for event_name in ["PostToolUse", "PostToolUseFailure"] {
            let payload = serde_json::json!({
                "hook_event_name": event_name,
                "session_id": "session-x",
                "tool_name": "Agent",
                "tool_use_id": "tool-4",
            })
            .to_string()
            .into_bytes();
            let handled = handle(&payload, &settings(), &PanicsIfAsked);
            assert!(
                matches!(handled.event, HookEvent::AgentToolCall(_)),
                "{event_name}: the gate was asked, and the panic was caught"
            );
            assert_eq!(handled.answer, HookAnswer::Silent);
            assert_eq!(handled.coordinator, None);
        }
    }

    /// A payload that is not JSON never reaches the coordinator and
    /// answers silent — the same "not ours" path `decide` already
    /// covers, exercised here through the wiring.
    #[test]
    fn a_non_json_payload_is_silent_and_unasked() {
        let handled = handle(b"not json at all {{{", &settings(), &PanicsIfAsked);
        assert_eq!(handled.answer, HookAnswer::Silent);
        assert_eq!(handled.event, HookEvent::NotOurs);
    }

    /// A panic anywhere inside `handle_inner` — here, inside the gate
    /// it was handed — never escapes `handle`: the hook this backs
    /// cannot leave the tool call it was watching unanswered.
    #[test]
    fn a_panicking_gate_still_answers_silent_rather_than_unwinding() {
        struct PanickingGate;
        impl Gate for PanickingGate {
            fn register_run(&self, _: &RunRegistration) -> Result<(), crate::admission::GateError> {
                unreachable!()
            }
            fn admit(
                &self,
                _: &DispatchRequest,
            ) -> Result<crate::admission::Decision, crate::admission::GateError> {
                panic!("the coordinator connection panicked")
            }
            fn bind(
                &self,
                _: &str,
                _: Option<&str>,
                _: Option<u32>,
            ) -> Result<crate::admission::BindOutcome, crate::admission::GateError> {
                unreachable!()
            }
            fn heartbeat(
                &self,
                _: &str,
            ) -> Result<crate::admission::HeartbeatStatus, crate::admission::GateError>
            {
                unreachable!()
            }
            fn mark_waiting(
                &self,
                _: &str,
            ) -> Result<crate::admission::WaitOutcome, crate::admission::GateError> {
                unreachable!()
            }
            fn resume(
                &self,
                _: &str,
            ) -> Result<crate::admission::ResumeOutcome, crate::admission::GateError> {
                unreachable!()
            }
            fn release(
                &self,
                _: &str,
            ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError>
            {
                unreachable!()
            }
            fn settle(
                &self,
                _: &str,
                _: Option<i64>,
            ) -> Result<crate::admission::LifecycleOutcome, crate::admission::GateError>
            {
                unreachable!()
            }
            fn withdraw(
                &self,
                _: &str,
            ) -> Result<crate::admission::WithdrawOutcome, crate::admission::GateError>
            {
                unreachable!()
            }
            fn enforcement(&self) -> crate::admission::Enforcement {
                crate::admission::Enforcement::InProcess
            }
        }
        let payload = spawn_payload("session-panics", "tool-5");
        let handled = handle(&payload, &settings(), &PanickingGate);
        assert_eq!(handled.answer, HookAnswer::Silent);
    }

    #[test]
    fn journal_entry_carries_the_raw_payload_and_the_decision() {
        use super::super::decide::{Refusal, RefusalRule};

        let payload = spawn_payload("session-j", "tool-j");
        let refusal = Refusal {
            rule: RefusalRule::Admission(crate::admission::Refusal::UnknownRun),
            reason: "run r1 is not registered".into(),
            remedy: "restart the session that started it".into(),
        };
        let handled = Handled {
            event: event::parse(&payload),
            coordinator: Some(crate::admission::Decision::Refused {
                code: crate::admission::Refusal::UnknownRun,
                detail: "run r1 is not registered".into(),
            }),
            answer: HookAnswer::Refuse {
                refusal: refusal.clone(),
            },
            waited: Duration::ZERO,
        };
        let entry = journal_entry(&payload, &handled);
        assert_eq!(entry["decision"], "refuse");
        // The journalled reason must be exactly what `stdout_payload`
        // tells the session, not a paraphrase of it.
        assert_eq!(
            entry["reason"].as_str().expect("reason"),
            refusal.sentence()
        );
        assert!(entry["reason"]
            .as_str()
            .expect("reason")
            .contains("not registered"));
        // The rule and availability the criteria asks for: present on a
        // refusal, and naming the admission code that actually fired.
        assert_eq!(entry["rule"], "admission_unknown_run");
        assert_eq!(entry["availability"], "decided");
        assert_eq!(entry["payload"]["session_id"], "session-j");
        assert!(entry["recorded_at"].as_str().is_some());
        assert_eq!(entry["waited_ms"], 0);
        assert_eq!(entry["outcome"], "immediate");
    }

    #[test]
    fn journal_entry_falls_back_to_a_lossy_string_for_non_json_payloads() {
        let handled = Handled {
            event: HookEvent::NotOurs,
            coordinator: None,
            answer: HookAnswer::Silent,
            waited: Duration::ZERO,
        };
        let entry = journal_entry(b"not json {{{", &handled);
        assert_eq!(entry["payload"], "not json {{{");
        assert_eq!(entry["decision"], "silent");
        assert!(entry["reason"].is_null());
    }

    /// A firing that waited and was then admitted is journalled as
    /// `admitted_after_waiting`, distinct from a firing that waited and
    /// was refused: without the two being told apart in the record,
    /// backpressure and stalling look identical in the only place either
    /// one is visible.
    #[test]
    fn journal_entry_distinguishes_admitted_after_waiting_from_refused_after_waiting() {
        let payload = spawn_payload("session-wait", "tool-wait");
        let admitted = Handled {
            event: event::parse(&payload),
            coordinator: Some(crate::admission::Decision::Granted),
            answer: HookAnswer::Silent,
            waited: Duration::from_millis(900),
        };
        let entry = journal_entry(&payload, &admitted);
        assert_eq!(entry["waited_ms"], 900);
        assert_eq!(entry["outcome"], "admitted_after_waiting");

        let refused = Handled {
            event: event::parse(&payload),
            coordinator: Some(crate::admission::Decision::Queued { position: 1 }),
            answer: HookAnswer::Refuse {
                refusal: super::super::decide::Refusal {
                    rule: super::super::decide::RefusalRule::QueueTimeout,
                    reason: "relais waited 2.0s for a seat before giving up".into(),
                    remedy: "retry when a running agent finishes".into(),
                },
            },
            waited: Duration::from_secs(2),
        };
        let entry = journal_entry(&payload, &refused);
        assert_eq!(entry["waited_ms"], 2_000);
        assert_eq!(entry["outcome"], "refused_after_waiting");
    }

    #[cfg(unix)]
    #[test]
    fn append_journal_creates_an_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_support::temp_dir("hook-journal");
        let path = dir.join("hook_journal.jsonl");
        let entry = serde_json::json!({"a": 1});
        append_journal(&path, &entry).expect("append");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "journal must be owner-only");
        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(text.trim(), entry.to_string());

        // A second firing appends a second line rather than truncating,
        // and the file stays owner-only.
        let second = serde_json::json!({"a": 2});
        append_journal(&path, &second).expect("append");
        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(text.lines().count(), 2);
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    // The test above calls `append_journal` sequentially, one write
    // fully finished before the next starts, so it could never observe
    // two firings landing at the same instant — which is exactly how the
    // interleaved-write bug reached a real session (issue #78: two
    // spawns whose firings collided wrote two lines that were not JSON).
    // This test instead fires many threads at the same file concurrently,
    // the way two hooks racing actually do.
    //
    // Confirmed red before it was trusted: with the body restored to
    // `writeln!(file, "{entry}")` this failed on five runs out of five.
    // A concurrency test that has only ever been seen green proves
    // nothing about the race it claims to cover.
    //
    // Not gated to unix: the Windows arm opens the same file with
    // `append(true)` and `FILE_APPEND_DATA` is per-write atomic in the
    // same way, so the race — and this regression — belong to both
    // platforms. Nothing in the body is platform-specific, unlike the
    // permission-mode test above.
    #[test]
    fn append_journal_survives_concurrent_writers() {
        let dir = crate::test_support::temp_dir("hook-journal-concurrent");
        let path = dir.join("hook_journal.jsonl");
        const THREADS: usize = 8;
        const PER_THREAD: usize = 50;
        std::thread::scope(|scope| {
            for thread_id in 0..THREADS {
                let path = &path;
                scope.spawn(move || {
                    for i in 0..PER_THREAD {
                        let entry = serde_json::json!({"thread": thread_id, "i": i});
                        append_journal(path, &entry).expect("append");
                    }
                });
            }
        });
        let text = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            THREADS * PER_THREAD,
            "every write must land as its own line"
        );
        for line in lines {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|err| panic!("line is not valid JSON: {err}: {line:?}"));
        }
    }

    /// A queued spawn a freed seat admits MID-WAIT comes back silent —
    /// the polling protocol works, not just its timeout path — and the
    /// polling that got it there never created a duplicate queue entry
    /// or a second reservation: afterward exactly one seat is held for
    /// the session and nothing is left queued.
    #[test]
    fn a_queued_spawn_a_freed_seat_admits_mid_wait_comes_back_silent() {
        let limits = ConcurrencyLimits {
            max_active_agents_per_session: Some(1),
            ..ConcurrencyLimits::default()
        };
        let gate = std::sync::Arc::new(LocalGate::new(limits));
        let session = "session-waits";

        let first = handle(
            &spawn_payload(session, "tool-1"),
            &settings(),
            gate.as_ref(),
        );
        assert_eq!(first.answer, HookAnswer::Silent, "tool-1 takes the seat");

        let first_dispatch =
            ids::derive_dispatch_id(&SessionId::new(session), &ToolUseId::new("tool-1"));
        let releaser_gate = gate.clone();
        let releaser = std::thread::spawn(move || {
            // Well inside the two-second wait budget below, and well
            // past `runner::ADMISSION_POLL` (250 ms), so the second
            // spawn is certainly still polling when this lands.
            std::thread::sleep(std::time::Duration::from_millis(300));
            releaser_gate
                .settle(first_dispatch.as_str(), None)
                .expect("settle");
            releaser_gate
                .release(first_dispatch.as_str())
                .expect("release");
        });

        let second = handle(
            &spawn_payload(session, "tool-2"),
            &waiting_settings(2),
            gate.as_ref(),
        );
        releaser.join().expect("releaser thread");

        assert_eq!(
            second.answer,
            HookAnswer::Silent,
            "the freed seat admits it before the wait runs out"
        );
        assert!(
            second.waited >= std::time::Duration::from_millis(200),
            "it actually waited rather than being admitted at once: {:?}",
            second.waited
        );
        assert!(
            second.waited < std::time::Duration::from_secs(2),
            "it was admitted before its budget ran out: {:?}",
            second.waited
        );

        let after = gate.status();
        assert_eq!(after.queued, 0, "no duplicate queue entry remains");
        assert_eq!(
            after.active_by_class.get(ResourceClass::ModelWork.as_str()),
            Some(&1),
            "exactly one seat is held afterward — no second reservation"
        );
    }

    /// A queued spawn that exhausts its whole wait budget is refused, and
    /// withdraws its own queue entry rather than leaving it behind for
    /// `admission::UNCLAIMED_GRACE` to clean up.
    #[test]
    fn a_queued_spawn_that_exhausts_its_wait_is_refused_and_withdraws_itself() {
        let limits = ConcurrencyLimits {
            max_active_agents_per_session: Some(1),
            ..ConcurrencyLimits::default()
        };
        let gate = LocalGate::new(limits);
        let session = "session-gives-up";
        let first = handle(&spawn_payload(session, "tool-1"), &settings(), &gate);
        assert_eq!(first.answer, HookAnswer::Silent);

        // Nothing ever frees tool-1's seat: the second spawn waits out
        // its whole budget and gives up.
        let second = handle(
            &spawn_payload(session, "tool-2"),
            &waiting_settings(1),
            &gate,
        );
        match &second.answer {
            HookAnswer::Refuse { refusal } => {
                let reason = refusal.sentence();
                assert!(reason.contains("waited"), "{reason}");
                assert!(!reason.contains("cannot hold a tool call open"), "{reason}");
                assert_eq!(
                    refusal.rule,
                    super::super::decide::RefusalRule::QueueTimeout
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(
            second.waited >= std::time::Duration::from_millis(900),
            "it waited out its whole budget: {:?}",
            second.waited
        );

        let status = gate.status();
        assert_eq!(
            status.queued, 0,
            "the exhausted wait withdrew its own queue entry"
        );
    }

    /// `queue_wait_secs = 0` refuses at once, exactly as before this
    /// package existed: no sleep, no poll.
    #[test]
    fn a_zero_queue_wait_refuses_at_once_with_no_wait() {
        let limits = ConcurrencyLimits {
            max_active_agents_per_session: Some(1),
            ..ConcurrencyLimits::default()
        };
        let gate = LocalGate::new(limits);
        let session = "session-refuses-at-once";
        let first = handle(&spawn_payload(session, "tool-1"), &settings(), &gate);
        assert_eq!(first.answer, HookAnswer::Silent);

        let started = std::time::Instant::now();
        let second = handle(&spawn_payload(session, "tool-2"), &settings(), &gate);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "queue_wait_secs = 0 must not sleep at all: {:?}",
            started.elapsed()
        );
        assert!(matches!(second.answer, HookAnswer::Refuse { .. }));
        assert_eq!(second.waited, std::time::Duration::ZERO);
    }
}
