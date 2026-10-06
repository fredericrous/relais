//! The hook's side of native dispatches (SPEC §23): the calls a firing
//! makes to the coordinator when relais asked the parent session for a
//! spawn or a continuation, and what the pure decisions in
//! [`decide`](super::decide) make of the answers.
//!
//! A firing is native when its call carries a run marker
//! ([`crate::native`]), when it is a message to an agent behind a native
//! dispatch, or when it is a `WorktreeCreate` or a `SubagentStop` that the
//! coordinator recognises as a native dispatch's. Everything else is
//! [`Native::Unconcerned`] and handled exactly as it always was.
//!
//! A marked call is NEVER admitted under the session's own run: it was
//! admitted under the native run when the runner sent its dispatch, so it
//! is charged once.

use serde_json::Value;

use super::decide::{self, HookAnswer, Recipient};
use super::event::{AgentToolCall, SendMessageCall, ToolCallPhase, WorktreeCreate};
use super::event::{HookEvent, SubagentStop};
use crate::admission::{BindNativeOutcome, ClaimOutcome, Gate, GateError};
use crate::native::{find_marker, Marker};
use crate::policy::HookAdmissionSettings;

/// Which native path a firing took, for the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePath {
    /// A marked spawn the coordinator claimed: rewritten as relais asked.
    ClaimedSpawn,
    /// A marked message the coordinator claimed: rewritten as relais asked.
    ClaimedContinue,
    /// A marked call the coordinator refused, or whose marker was
    /// ambiguous.
    ClaimRefused,
    /// A marked call with the coordinator unreachable: refused.
    ClaimUnreachable,
    /// A marked spawn returned and bound its agent.
    Bound,
    /// A marked spawn returned and the dispatch failed, or never expected
    /// the bind; the dispatch carries the reason.
    BindFailed,
    /// A marked spawn returned and the coordinator did not answer, or
    /// there was no agent to bind.
    BindSkipped,
    /// A `WorktreeCreate` answered with relais's tree.
    NativeTree,
    /// A `WorktreeCreate` no native dispatch claimed: the default tree.
    NotNativeTree,
    /// A `SubagentStop` of a native dispatch's agent.
    NativeStop,
    /// A `SubagentStop` no native dispatch claimed: today's handling.
    NotNativeStop,
    /// An unmarked message to the agent behind a native dispatch: refused.
    MessageBehindRelais,
    /// An unmarked message to any other agent.
    MessageUnconcerned,
}

impl NativePath {
    pub fn label(self) -> &'static str {
        match self {
            Self::ClaimedSpawn => "claimed_spawn",
            Self::ClaimedContinue => "claimed_continue",
            Self::ClaimRefused => "claim_refused",
            Self::ClaimUnreachable => "claim_unreachable",
            Self::Bound => "bound",
            Self::BindFailed => "bind_failed",
            Self::BindSkipped => "bind_skipped",
            Self::NativeTree => "native_tree",
            Self::NotNativeTree => "not_native_tree",
            Self::NativeStop => "native_stop",
            Self::NotNativeStop => "not_native_stop",
            Self::MessageBehindRelais => "message_behind_relais",
            Self::MessageUnconcerned => "message_unconcerned",
        }
    }
}

/// What the native side made of one firing.
#[derive(Debug)]
pub enum Native {
    /// Nothing native about it: ordinary handling.
    Unconcerned,
    /// Answered here; ordinary handling does not run.
    Answered {
        answer: HookAnswer,
        path: NativePath,
    },
    /// Asked, and not a native dispatch's: ordinary handling runs, and the
    /// journal records the question.
    Passed { path: NativePath },
}

/// Handle one firing's native side. `gate` is `None` when the machine has
/// no coordinator to ask at all, which reads as unreachable.
pub fn handle(
    event: &HookEvent,
    settings: &HookAdmissionSettings,
    gate: Option<&dyn Gate>,
) -> Native {
    match event {
        HookEvent::AgentToolCall(call) => agent_call(call, gate),
        HookEvent::SendMessageCall(call) => message_call(call, settings, gate),
        HookEvent::WorktreeCreate(create) => worktree(create, gate),
        HookEvent::SubagentStop(stop) => subagent_stop(stop, gate),
        HookEvent::SessionStart(_)
        | HookEvent::SessionEnd(_)
        | HookEvent::SubagentStart(_)
        | HookEvent::NotOurs => Native::Unconcerned,
    }
}

/// One coordinator call. Any failure to make it is `None`, which every
/// decision reads as "could not be reached": a marked call is refused for
/// it, and an unmarked one follows the machine's own stance.
fn ask<T>(
    gate: Option<&dyn Gate>,
    call: impl FnOnce(&dyn Gate) -> Result<T, GateError>,
) -> Option<T> {
    // The error says why the call failed; what the hook does about it does
    // not depend on the reason, and a hook has nowhere to report it.
    gate.and_then(|gate| call(gate).ok())
}

fn text_of<'a>(input: Option<&'a Value>, key: &str) -> &'a str {
    input
        .and_then(|input| input.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn claim_path(claim: &Option<ClaimOutcome>) -> NativePath {
    match claim {
        Some(ClaimOutcome::Spawn { .. }) => NativePath::ClaimedSpawn,
        Some(ClaimOutcome::Continue { .. }) => NativePath::ClaimedContinue,
        Some(
            ClaimOutcome::UnknownDispatch
            | ClaimOutcome::WrongSession
            | ClaimOutcome::AlreadyClaimed
            | ClaimOutcome::Finished
            | ClaimOutcome::Cancelled
            | ClaimOutcome::LeaseLapsed
            | ClaimOutcome::WrongRecipient
            | ClaimOutcome::NotLeasable,
        ) => NativePath::ClaimRefused,
        None => NativePath::ClaimUnreachable,
    }
}

fn agent_call(call: &AgentToolCall, gate: Option<&dyn Gate>) -> Native {
    let marker = find_marker(text_of(call.tool_input.as_ref(), "prompt"));
    let dispatch_id = match marker {
        Marker::None => return Native::Unconcerned,
        Marker::Ambiguous => {
            return match call.phase {
                ToolCallPhase::Pre => Native::Answered {
                    answer: decide::decide_ambiguous_marker(),
                    path: NativePath::ClaimRefused,
                },
                ToolCallPhase::Post { .. } | ToolCallPhase::PostFailure => Native::Answered {
                    answer: HookAnswer::Silent,
                    path: NativePath::BindSkipped,
                },
            };
        }
        Marker::One(id) => id,
    };
    match &call.phase {
        ToolCallPhase::Pre => {
            let claim = ask(gate, |gate| {
                gate.claim_native(call.session_id.as_str(), &dispatch_id, None)
            });
            Native::Answered {
                path: claim_path(&claim),
                answer: decide::decide_marked_spawn(call.tool_input.as_ref(), claim),
            }
        }
        ToolCallPhase::Post {
            launched_agent: Some(agent_id),
        } => {
            let bound = ask(gate, |gate| {
                gate.bind_native(call.session_id.as_str(), &dispatch_id, agent_id.as_str())
            });
            let path = match bound {
                Some(BindNativeOutcome::Bound) => NativePath::Bound,
                Some(BindNativeOutcome::Failed { .. }) => NativePath::BindFailed,
                Some(BindNativeOutcome::NotNative) | None => NativePath::BindSkipped,
            };
            Native::Answered {
                answer: HookAnswer::Silent,
                path,
            }
        }
        ToolCallPhase::Post {
            launched_agent: None,
        }
        | ToolCallPhase::PostFailure => Native::Answered {
            answer: HookAnswer::Silent,
            path: NativePath::BindSkipped,
        },
    }
}

fn message_call(
    call: &SendMessageCall,
    settings: &HookAdmissionSettings,
    gate: Option<&dyn Gate>,
) -> Native {
    // Only a `Pre` holds the call open long enough to say anything.
    match call.phase {
        ToolCallPhase::Pre => {}
        ToolCallPhase::Post { .. } | ToolCallPhase::PostFailure => return Native::Unconcerned,
    }
    match find_marker(call.message.as_deref().unwrap_or("")) {
        Marker::Ambiguous => Native::Answered {
            answer: decide::decide_ambiguous_marker(),
            path: NativePath::ClaimRefused,
        },
        Marker::One(dispatch_id) => {
            let claim = match &call.to {
                Some(to) => ask(gate, |gate| {
                    gate.claim_native(call.session_id.as_str(), &dispatch_id, Some(to))
                }),
                // A marked message that names no recipient is not the
                // continuation relais asked for, and asking would only
                // risk claiming a spawn.
                None => Some(ClaimOutcome::WrongRecipient),
            };
            Native::Answered {
                path: claim_path(&claim),
                answer: decide::decide_marked_message(call.tool_input.as_ref(), claim),
            }
        }
        Marker::None => {
            let Some(to) = &call.to else {
                return Native::Unconcerned;
            };
            let recipient = ask(gate, |gate| {
                gate.is_native_agent(call.session_id.as_str(), to)
            })
            .map(|native| {
                if native {
                    Recipient::RelaisWorker
                } else {
                    Recipient::Other
                }
            });
            let path = match recipient {
                Some(Recipient::RelaisWorker) => NativePath::MessageBehindRelais,
                Some(Recipient::Other) | None => NativePath::MessageUnconcerned,
            };
            Native::Answered {
                answer: decide::decide_unmarked_message(settings, recipient),
                path,
            }
        }
    }
}

fn worktree(create: &WorktreeCreate, gate: Option<&dyn Gate>) -> Native {
    let outcome = ask(gate, |gate| {
        gate.native_worktree(create.session_id.as_str(), &create.name)
    });
    match decide::decide_native_tree(outcome) {
        Some(answer) => Native::Answered {
            answer,
            path: NativePath::NativeTree,
        },
        None => Native::Passed {
            path: NativePath::NotNativeTree,
        },
    }
}

fn subagent_stop(stop: &SubagentStop, gate: Option<&dyn Gate>) -> Native {
    let outcome = ask(gate, |gate| {
        gate.stop_native(
            stop.session_id.as_str(),
            stop.agent_id.as_str(),
            stop.agent_transcript_path.as_deref(),
            stop.last_assistant_message.as_deref(),
        )
    });
    match decide::decide_native_stop(outcome) {
        Some(answer) => Native::Answered {
            answer,
            path: NativePath::NativeStop,
        },
        None => Native::Passed {
            path: NativePath::NotNativeStop,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::admission::{
        AdmissionState, BindOutcome, Decision, DispatchRequest, DispatchSource, Enforcement,
        HeartbeatStatus, LifecycleOutcome, LocalGate, NativeAsk, NativeProgress, NativeState,
        RegisterOutcome, ResourceClass, ResumeOutcome, RunRegistration, WaitOutcome,
        WithdrawOutcome,
    };
    use crate::hook::respond::{handle_in, journal_entry, Handled};
    use crate::native::marker_line;
    use crate::policy::{ConcurrencyLimits, CoordinatorUnreachableBehavior};

    const SESSION: &str = "session-0000";
    const RUN: &str = "run-native";
    const TREE: &str = "/relais/tree/task";

    fn settings_with(behaviour: CoordinatorUnreachableBehavior) -> HookAdmissionSettings {
        HookAdmissionSettings {
            binding_lease_secs: 120,
            dispatch_reserve_micros: crate::money::MicroUsd::ZERO,
            on_coordinator_unreachable: behaviour,
            queue_wait_secs: 0,
        }
    }

    fn carry_on() -> HookAdmissionSettings {
        settings_with(CoordinatorUnreachableBehavior::CarryOn)
    }

    /// A recorded payload with `patch` applied to it.
    fn payload(recorded: &str, patch: impl FnOnce(&mut Value)) -> Vec<u8> {
        let mut value: Value = serde_json::from_str(recorded).expect("fixture parses");
        patch(&mut value);
        value.to_string().into_bytes()
    }

    const PRE_AGENT: &str = include_str!("../../tests/fixtures/hooks-native/0001-PreToolUse.json");
    const WORKTREE_CREATE: &str =
        include_str!("../../tests/fixtures/hooks-native/0002-WorktreeCreate.json");
    const POST_AGENT: &str =
        include_str!("../../tests/fixtures/hooks-native/0004-PostToolUse.json");
    const STOP: &str = include_str!("../../tests/fixtures/hooks-native/0005-SubagentStop.json");
    const PRE_MESSAGE: &str =
        include_str!("../../tests/fixtures/hooks-native/0006-PreToolUse.json");

    fn marked_prompt(dispatch: &str) -> String {
        format!("Run pwd with Bash.\n{}", marker_line(dispatch))
    }

    /// The spawn's `PreToolUse`, carrying `dispatch`'s marker and asking
    /// for another model than relais did.
    fn pre_spawn(dispatch: &str) -> Vec<u8> {
        payload(PRE_AGENT, |value| {
            value["tool_input"]["prompt"] = marked_prompt(dispatch).into();
            value["tool_input"]["model"] = "opus".into();
        })
    }

    fn post_spawn(dispatch: &str, agent: &str) -> Vec<u8> {
        payload(POST_AGENT, |value| {
            value["tool_input"]["prompt"] = marked_prompt(dispatch).into();
            value["tool_response"]["agentId"] = agent.into();
        })
    }

    fn worktree_create(name: &str, cwd: &Path) -> Vec<u8> {
        payload(WORKTREE_CREATE, |value| {
            value["name"] = name.into();
            value["cwd"] = cwd.to_str().expect("utf-8 path").into();
        })
    }

    fn stop(agent: &str, last: &str) -> Vec<u8> {
        payload(STOP, |value| {
            value["agent_id"] = agent.into();
            value["last_assistant_message"] = last.into();
        })
    }

    fn message(to: &str, text: &str) -> Vec<u8> {
        payload(PRE_MESSAGE, |value| {
            value["tool_input"]["to"] = to.into();
            value["tool_input"]["message"] = text.into();
        })
    }

    fn spawn_ask(dispatch: &str) -> NativeAsk {
        NativeAsk::Spawn {
            subagent_type: "relais-worker-sonnet-medium".into(),
            model: "sonnet".into(),
            prompt: marked_prompt(dispatch),
            worktree: PathBuf::from(TREE),
        }
    }

    fn continue_ask(dispatch: &str, agent: &str) -> NativeAsk {
        NativeAsk::Continue {
            agent_id: agent.into(),
            message: format!("Fix the failing check.\n{}", marker_line(dispatch)),
        }
    }

    fn dispatch_request(dispatch: &str) -> DispatchRequest {
        DispatchRequest {
            dispatch_id: dispatch.into(),
            run_id: RUN.into(),
            session_id: SESSION.into(),
            parent_dispatch: None,
            depth: 0,
            resource: ResourceClass::ModelWork,
            reserve_micros: 5_000,
            source: DispatchSource::ManagedRun,
            caller_agent_id: None,
        }
    }

    fn admit(gate: &dyn Gate, dispatch: &str) {
        gate.register_run(&RunRegistration {
            run_id: RUN.into(),
            session_id: SESSION.into(),
            budget_micros: None,
            max_agents: None,
            max_depth: None,
        })
        .expect("run registers");
        assert_eq!(
            gate.admit(&dispatch_request(dispatch)).expect("admits"),
            Decision::Granted
        );
    }

    /// What the runner does before it asks for a native dispatch: the run,
    /// the admitted dispatch, and the registered ask.
    fn request(gate: &dyn Gate, dispatch: &str, ask: &NativeAsk) {
        admit(gate, dispatch);
        assert_eq!(
            gate.register_native(SESSION, dispatch, ask)
                .expect("registers"),
            RegisterOutcome::Registered
        );
    }

    fn fire(gate: &dyn Gate, bytes: &[u8]) -> Handled {
        handle_in(bytes, &carry_on(), gate, None)
    }

    fn state_of(gate: &dyn Gate, dispatch: &str) -> NativeState {
        match gate.native_status(dispatch).expect("status") {
            NativeProgress::Known { state } => state,
            other => panic!("expected a record for {dispatch}, got {other:?}"),
        }
    }

    fn refusal_label(handled: &Handled) -> String {
        match &handled.answer {
            HookAnswer::Refuse { refusal } => refusal.rule.label(),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// A gate over a shared state with a clock the test moves, counting the
    /// admissions asked of it.
    struct Probe {
        state: Mutex<AdmissionState>,
        start: Instant,
        elapsed: Mutex<Duration>,
        admits: AtomicUsize,
    }

    impl Probe {
        fn new() -> Self {
            Self {
                state: Mutex::new(AdmissionState::new(ConcurrencyLimits::default())),
                start: Instant::now(),
                elapsed: Mutex::new(Duration::ZERO),
                admits: AtomicUsize::new(0),
            }
        }

        fn advance(&self, by: Duration) {
            *self.elapsed.lock().unwrap() += by;
        }

        fn now(&self) -> Instant {
            self.start + *self.elapsed.lock().unwrap()
        }

        fn state(&self) -> std::sync::MutexGuard<'_, AdmissionState> {
            self.state.lock().unwrap()
        }
    }

    type GateResult<T> = Result<T, crate::admission::GateError>;

    impl Gate for Probe {
        fn register_run(&self, registration: &RunRegistration) -> GateResult<()> {
            self.state().register_run(registration, self.now());
            Ok(())
        }
        fn admit(&self, request: &DispatchRequest) -> GateResult<Decision> {
            self.admits.fetch_add(1, Ordering::SeqCst);
            Ok(self.state().request(request, self.now()))
        }
        fn bind(&self, _: &str, _: Option<&str>, _: Option<u32>) -> GateResult<BindOutcome> {
            unreachable!()
        }
        fn heartbeat(&self, dispatch_id: &str) -> GateResult<HeartbeatStatus> {
            Ok(self.state().heartbeat(dispatch_id, self.now()))
        }
        fn mark_waiting(&self, _: &str) -> GateResult<WaitOutcome> {
            unreachable!()
        }
        fn resume(&self, _: &str) -> GateResult<ResumeOutcome> {
            unreachable!()
        }
        fn release(&self, _: &str) -> GateResult<LifecycleOutcome> {
            unreachable!()
        }
        fn settle(&self, _: &str, _: Option<i64>) -> GateResult<LifecycleOutcome> {
            unreachable!()
        }
        fn withdraw(&self, _: &str) -> GateResult<WithdrawOutcome> {
            unreachable!()
        }
        fn register_native(
            &self,
            session_id: &str,
            dispatch_id: &str,
            ask: &NativeAsk,
        ) -> GateResult<RegisterOutcome> {
            Ok(self.state().register_native(session_id, dispatch_id, ask))
        }
        fn claim_native(
            &self,
            session_id: &str,
            dispatch_id: &str,
            to: Option<&str>,
        ) -> GateResult<ClaimOutcome> {
            Ok(self
                .state()
                .claim_native(session_id, dispatch_id, to, self.now()))
        }
        fn bind_native(
            &self,
            session_id: &str,
            dispatch_id: &str,
            agent_id: &str,
        ) -> GateResult<BindNativeOutcome> {
            Ok(self
                .state()
                .bind_native(session_id, dispatch_id, agent_id, self.now()))
        }
        fn native_status(&self, dispatch_id: &str) -> GateResult<NativeProgress> {
            Ok(self.state().native_status(dispatch_id))
        }
        fn enforcement(&self) -> Enforcement {
            Enforcement::InProcess
        }
    }

    /// A coordinator that cannot be reached: every native call fails.
    struct Down;

    fn down(operation: &'static str) -> crate::admission::GateError {
        crate::admission::GateError::Unavailable {
            operation,
            socket: "none".into(),
            cause: Box::new(std::io::Error::other("no daemon")),
        }
    }

    impl Gate for Down {
        fn register_run(&self, _: &RunRegistration) -> GateResult<()> {
            Err(down("register_run"))
        }
        fn admit(&self, _: &DispatchRequest) -> GateResult<Decision> {
            Err(down("admit"))
        }
        fn bind(&self, _: &str, _: Option<&str>, _: Option<u32>) -> GateResult<BindOutcome> {
            Err(down("bind"))
        }
        fn heartbeat(&self, _: &str) -> GateResult<HeartbeatStatus> {
            Err(down("heartbeat"))
        }
        fn mark_waiting(&self, _: &str) -> GateResult<WaitOutcome> {
            Err(down("mark_waiting"))
        }
        fn resume(&self, _: &str) -> GateResult<ResumeOutcome> {
            Err(down("resume"))
        }
        fn release(&self, _: &str) -> GateResult<LifecycleOutcome> {
            Err(down("release"))
        }
        fn settle(&self, _: &str, _: Option<i64>) -> GateResult<LifecycleOutcome> {
            Err(down("settle"))
        }
        fn withdraw(&self, _: &str) -> GateResult<WithdrawOutcome> {
            Err(down("withdraw"))
        }
        fn claim_native(&self, _: &str, _: &str, _: Option<&str>) -> GateResult<ClaimOutcome> {
            Err(down("claim_native"))
        }
        fn is_native_agent(&self, _: &str, _: &str) -> GateResult<bool> {
            Err(down("is_native_agent"))
        }
        fn enforcement(&self) -> Enforcement {
            Enforcement::InProcess
        }
    }

    /// A registered spawn's whole life through the hook, against a real
    /// `LocalGate` and the payloads of a real session: the rewrite runs
    /// relais's spawn, the tree is relais's, the bind and the stop reach
    /// the record, and the dispatch is left for the runner to settle.
    #[test]
    fn a_registered_spawn_runs_as_relais_asked_from_claim_to_stop() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        request(&gate, "d1", &spawn_ask("d1"));

        let claimed = fire(&gate, &pre_spawn("d1"));
        assert_eq!(claimed.native, Some(NativePath::ClaimedSpawn));
        let HookAnswer::Rewrite { input } = &claimed.answer else {
            panic!("expected a rewrite, got {:?}", claimed.answer);
        };
        assert_eq!(input["subagent_type"], "relais-worker-sonnet-medium");
        assert_eq!(input["model"], "sonnet", "the call asked for opus");
        assert_eq!(input["prompt"], marked_prompt("d1"));
        assert_eq!(input["isolation"], "worktree");
        assert_eq!(input["description"], "e12 worker", "other fields are kept");
        let printed = claimed.answer.stdout_payload().expect("a rewrite prints");
        assert!(printed.contains("updatedInput"), "{printed}");
        assert_eq!(journal_entry(b"{}", &claimed)["native"], "claimed_spawn");

        let tree = fire(
            &gate,
            &worktree_create("agent-agent-01", Path::new("/REPO")),
        );
        assert_eq!(tree.answer, HookAnswer::WorktreePath { path: TREE.into() });
        assert_eq!(tree.native, Some(NativePath::NativeTree));

        let bound = fire(&gate, &post_spawn("d1", "agent-01"));
        assert_eq!(bound.answer, HookAnswer::Silent);
        assert_eq!(bound.native, Some(NativePath::Bound));
        assert_eq!(
            state_of(&gate, "d1"),
            NativeState::Bound {
                agent_id: "agent-01".into()
            }
        );

        let stopped = fire(&gate, &stop("agent-01", "all done"));
        assert_eq!(stopped.answer, HookAnswer::Silent);
        assert_eq!(stopped.native, Some(NativePath::NativeStop));
        assert_eq!(
            state_of(&gate, "d1"),
            NativeState::Stopped {
                agent_id: "agent-01".into(),
                transcript_path: Some("/REDACTED/transcript.jsonl".into()),
                last_assistant_message: Some("all done".into()),
            }
        );
        let run = &gate.status().runs[RUN];
        assert_eq!(run.settled_micros, 0, "the runner settles, not the hook");
        assert_eq!(run.reserved_micros, 5_000, "the reservation still stands");
        assert_eq!(run.uncertain_settlements, 0);
    }

    #[test]
    fn a_marked_call_is_never_admitted_under_the_sessions_own_run() {
        let probe = Probe::new();
        request(&probe, "d1", &spawn_ask("d1"));
        probe.admits.store(0, Ordering::SeqCst);

        let claimed = fire(&probe, &pre_spawn("d1"));
        assert!(matches!(claimed.answer, HookAnswer::Rewrite { .. }));
        fire(&probe, &post_spawn("d1", "agent-01"));
        assert_eq!(
            probe.admits.load(Ordering::SeqCst),
            0,
            "the dispatch was admitted under the native run when the runner sent it"
        );

        let unmarked = fire(&probe, PRE_AGENT.as_bytes());
        assert_eq!(unmarked.answer, HookAnswer::Silent);
        assert_eq!(unmarked.native, None);
        assert!(
            probe.admits.load(Ordering::SeqCst) >= 1,
            "an unmarked spawn is admitted under the session's own run, as before"
        );
    }

    #[test]
    fn an_unmarked_spawn_is_admitted_exactly_as_before() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        let handled = fire(&gate, PRE_AGENT.as_bytes());
        assert_eq!(handled.answer, HookAnswer::Silent);
        assert!(matches!(handled.coordinator, Some(Decision::Granted)));
        assert_eq!(handled.native, None);
    }

    #[test]
    fn a_marker_no_one_registered_is_refused() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        let handled = fire(&gate, &pre_spawn("nobody"));
        assert_eq!(refusal_label(&handled), "native_unknown_dispatch");
        assert_eq!(handled.native, Some(NativePath::ClaimRefused));
    }

    #[test]
    fn a_second_spawn_for_one_dispatch_is_refused() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        request(&gate, "d1", &spawn_ask("d1"));
        assert!(matches!(
            fire(&gate, &pre_spawn("d1")).answer,
            HookAnswer::Rewrite { .. }
        ));
        let again = fire(&gate, &pre_spawn("d1"));
        assert_eq!(refusal_label(&again), "native_already_claimed");
    }

    #[test]
    fn a_finished_dispatch_is_refused() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        request(&gate, "d1", &spawn_ask("d1"));
        gate.settle("d1", Some(1)).expect("settles");
        gate.release("d1").expect("releases");
        let handled = fire(&gate, &pre_spawn("d1"));
        assert_eq!(refusal_label(&handled), "native_finished");
    }

    #[test]
    fn a_dispatch_whose_heartbeats_stopped_is_refused_and_one_still_heartbeating_is_not() {
        let probe = Probe::new();
        request(&probe, "d1", &spawn_ask("d1"));
        let ask = spawn_ask("d2");
        gate_admit_second(&probe, "d2", &ask);
        // d2 is still heartbeated at 100 s; d1 is not heard from again.
        probe.advance(Duration::from_secs(100));
        probe.heartbeat("d2").expect("heartbeats");
        probe.advance(Duration::from_secs(30));

        let lapsed = fire(&probe, &pre_spawn("d1"));
        assert_eq!(refusal_label(&lapsed), "native_lease_lapsed");
        let alive = fire(&probe, &pre_spawn("d2"));
        assert!(
            matches!(alive.answer, HookAnswer::Rewrite { .. }),
            "{:?}",
            alive.answer
        );
    }

    /// A second dispatch of the run already registered by `request`.
    fn gate_admit_second(gate: &dyn Gate, dispatch: &str, ask: &NativeAsk) {
        assert_eq!(
            gate.admit(&dispatch_request(dispatch)).expect("admits"),
            Decision::Granted
        );
        assert_eq!(
            gate.register_native(SESSION, dispatch, ask)
                .expect("registers"),
            RegisterOutcome::Registered
        );
    }

    #[test]
    fn an_ambiguous_marker_is_refused() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        request(&gate, "d1", &spawn_ask("d1"));
        let both = format!("{}\n{}", marker_line("d1"), marker_line("d2"));
        let handled = fire(
            &gate,
            &payload(PRE_AGENT, |value| {
                value["tool_input"]["prompt"] = both.into();
            }),
        );
        assert_eq!(refusal_label(&handled), "native_ambiguous_marker");
        assert_eq!(
            state_of(&gate, "d1"),
            NativeState::Requested {
                ask: spawn_ask("d1")
            },
            "an ambiguous call claims nothing"
        );
    }

    #[test]
    fn a_marked_call_is_refused_when_the_coordinator_is_unreachable_whatever_the_stance() {
        for behaviour in [
            CoordinatorUnreachableBehavior::CarryOn,
            CoordinatorUnreachableBehavior::Refuse,
        ] {
            let settings = settings_with(behaviour);
            let spawn = handle_in(&pre_spawn("d1"), &settings, &Down, None);
            assert_eq!(refusal_label(&spawn), "native_coordinator_unreachable");
            assert_eq!(spawn.native, Some(NativePath::ClaimUnreachable));
            let continued = handle_in(
                &message("agent-01", &marker_line("d2")),
                &settings,
                &Down,
                None,
            );
            assert_eq!(refusal_label(&continued), "native_coordinator_unreachable");
        }
        let without_home = crate::hook::respond::answer_without_home(&pre_spawn("d1"), &carry_on());
        assert!(matches!(without_home, HookAnswer::Refuse { .. }));
    }

    #[test]
    fn an_unmarked_call_keeps_the_machines_stance_when_the_coordinator_is_unreachable() {
        let carry = handle_in(&message("agent-01", "hello"), &carry_on(), &Down, None);
        assert_eq!(carry.answer, HookAnswer::Silent);
        let refuse = handle_in(
            &message("agent-01", "hello"),
            &settings_with(CoordinatorUnreachableBehavior::Refuse),
            &Down,
            None,
        );
        assert_eq!(refusal_label(&refuse), "coordinator_unreachable");
    }

    #[test]
    fn a_continuation_runs_as_relais_asked_and_only_for_its_agent() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        request(&gate, "d1", &spawn_ask("d1"));
        fire(&gate, &pre_spawn("d1"));
        fire(
            &gate,
            &worktree_create("agent-agent-01", Path::new("/REPO")),
        );
        fire(&gate, &post_spawn("d1", "agent-01"));
        fire(&gate, &stop("agent-01", "first attempt"));

        let ask = continue_ask("d2", "agent-01");
        gate_admit_second(&gate, "d2", &ask);
        let NativeAsk::Continue {
            message: relais, ..
        } = &ask
        else {
            unreachable!()
        };

        let elsewhere = fire(&gate, &message("agent-02", relais));
        assert_eq!(refusal_label(&elsewhere), "native_wrong_recipient");

        let claimed = fire(&gate, &message("agent-01", relais));
        assert_eq!(claimed.native, Some(NativePath::ClaimedContinue));
        let HookAnswer::Rewrite { input } = &claimed.answer else {
            panic!("expected a rewrite, got {:?}", claimed.answer);
        };
        assert_eq!(input["to"], "agent-01");
        assert_eq!(input["message"], relais.as_str());
        assert_eq!(input["summary"], "Run second command", "other fields stay");
        assert_eq!(
            state_of(&gate, "d2"),
            NativeState::Bound {
                agent_id: "agent-01".into()
            }
        );

        let second_stop = fire(&gate, &stop("agent-01", "second attempt"));
        assert_eq!(second_stop.native, Some(NativePath::NativeStop));
        assert_eq!(
            state_of(&gate, "d2"),
            NativeState::Stopped {
                agent_id: "agent-01".into(),
                transcript_path: Some("/REDACTED/transcript.jsonl".into()),
                last_assistant_message: Some("second attempt".into()),
            }
        );
        let NativeState::Stopped {
            last_assistant_message,
            ..
        } = state_of(&gate, "d1")
        else {
            panic!("the first attempt stays stopped");
        };
        assert_eq!(last_assistant_message.as_deref(), Some("first attempt"));
    }

    #[test]
    fn a_message_to_relais_worker_behind_its_back_is_refused_and_others_are_silent() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        request(&gate, "d1", &spawn_ask("d1"));
        fire(&gate, &pre_spawn("d1"));
        fire(
            &gate,
            &worktree_create("agent-agent-01", Path::new("/REPO")),
        );
        fire(&gate, &post_spawn("d1", "agent-01"));

        let behind = fire(&gate, &message("agent-01", "just do it my way"));
        assert_eq!(refusal_label(&behind), "native_behind_relais");
        assert_eq!(behind.native, Some(NativePath::MessageBehindRelais));
        let other = fire(&gate, &message("agent-02", "hello"));
        assert_eq!(other.answer, HookAnswer::Silent);
    }

    #[test]
    fn a_tree_named_for_another_agent_fails_the_dispatch() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        request(&gate, "d1", &spawn_ask("d1"));
        fire(&gate, &pre_spawn("d1"));
        fire(
            &gate,
            &worktree_create("agent-agent-01", Path::new("/REPO")),
        );
        let bound = fire(&gate, &post_spawn("d1", "agent-02"));
        assert_eq!(bound.native, Some(NativePath::BindFailed));
        assert_eq!(
            state_of(&gate, "d1"),
            NativeState::Failed {
                reason: "native_tree_mismatch".into()
            }
        );
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let status = crate::workspace::git_command(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn a_worktree_create_with_no_claimed_spawn_makes_the_default_tree() {
        let scratch = crate::test_support::temp_dir("native-default-tree");
        let repo = scratch.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        git_ok(
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "chore: init"],
        );
        let gate = LocalGate::new(ConcurrencyLimits::default());
        // Registered but not claimed: no tree is relais's yet.
        request(&gate, "d1", &spawn_ask("d1"));

        let handled = handle_in(
            &worktree_create("agent-agent-07", &repo),
            &carry_on(),
            &gate,
            Some(&scratch.join("records")),
        );
        let HookAnswer::WorktreePath { path } = &handled.answer else {
            panic!("expected a path, got {:?}", handled.answer);
        };
        assert!(path.ends_with(".claude/worktrees/agent-agent-07"));
        assert_eq!(handled.native, Some(NativePath::NotNativeTree));
    }
}
