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

use super::decide::{decide_or_silent, CoordinatorAnswer, HookAnswer};
use super::event::{self, HookEvent, ToolCallPhase};
use crate::admission::{DispatchRequest, Gate, ResourceClass};
use crate::ids;
use crate::policy::HookAdmissionSettings;

/// What one hook firing did, end to end: the event a payload classified
/// as, whatever the coordinator answered (only ever `Some` for a
/// `PreToolUse` spawn), and the decision that came of it.
#[derive(Debug)]
pub struct Handled {
    pub event: HookEvent,
    pub coordinator: CoordinatorAnswer,
    pub answer: HookAnswer,
}

/// Handle one hook payload: parse it, ask the coordinator about a
/// spawn, apply the existing pure decision, and — on a refusal —
/// withdraw the request this call may have made, so a spawn relais
/// refuses is never left holding a seat or a reservation.
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
    })
}

fn handle_inner(payload: &[u8], settings: &HookAdmissionSettings, gate: &dyn Gate) -> Handled {
    let event = event::parse(payload);
    let coordinator = ask_coordinator(&event, settings, gate);
    let answer = decide_or_silent(&event, settings, coordinator.clone());
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
    }
}

/// Ask the coordinator about one spawn. Only a `PreToolUse` on the
/// Agent tool is ever asked about: every other event is answered
/// `None` without a call, because [`decide`](super::decide::decide)
/// never refuses one and a request this function has no later chance
/// to release would otherwise reserve a seat for nothing.
fn ask_coordinator(
    event: &HookEvent,
    settings: &HookAdmissionSettings,
    gate: &dyn Gate,
) -> CoordinatorAnswer {
    let HookEvent::AgentToolCall(call) = event else {
        return None;
    };
    if call.phase != ToolCallPhase::Pre {
        return None;
    }
    let dispatch_id = ids::derive_dispatch_id(&call.session_id, &call.tool_use_id);
    let run_id = ids::derive_run_id(&call.session_id);
    let request = DispatchRequest {
        dispatch_id: dispatch_id.as_str().to_string(),
        run_id: run_id.as_str().to_string(),
        session_id: call.session_id.as_str().to_string(),
        // Nothing in a single hook payload joins the tool call that
        // admitted an agent to the dispatch it descends from (see
        // `hook::event`'s module doc), so a parent this coordinator
        // could enforce depth against is not knowable here.
        parent_dispatch: None,
        depth: 0,
        resource: ResourceClass::ModelWork,
        reserve_micros: settings.dispatch_reserve_micros.to_micros(),
    };
    // Any failure to reach the coordinator — no daemon, a stale
    // socket, a protocol mismatch — is exactly `CoordinatorAnswer`'s
    // `None`: "could not be reached at all", handled by
    // `HookAdmissionSettings::on_coordinator_unreachable` inside
    // `decide` rather than here.
    gate.admit(&request).ok()
}

/// What one firing is journalled as: what arrived, what the
/// coordinator answered (when it was asked), and what was decided.
/// Pure — it builds the record, [`append_journal`] writes it.
pub fn journal_entry(payload: &[u8], handled: &Handled) -> serde_json::Value {
    // The raw payload, not a re-serialization of the typed event: a
    // payload this crate does not model (a future harness field, or
    // something that failed to parse at all) is still what arrived,
    // and `event` below already records what relais made of it.
    let payload_value = serde_json::from_slice::<serde_json::Value>(payload).unwrap_or_else(|_| {
        serde_json::Value::String(String::from_utf8_lossy(payload).into_owned())
    });
    let (decision, reason) = match &handled.answer {
        HookAnswer::Silent => ("silent", None),
        HookAnswer::Refuse { reason } => ("refuse", Some(reason.clone())),
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
    })
}

/// Append one journal entry as a single JSON line, creating the file
/// owner-only if it does not exist yet. A payload names a person's
/// transcript path and working directory (SPEC §23), so the file this
/// writes to is never created with group or other access.
pub fn append_journal(path: &Path, entry: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = open_owner_only(path)?;
    writeln!(file, "{entry}")
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
    use crate::ids::SessionId;
    use crate::policy::{ConcurrencyLimits, CoordinatorUnreachableBehavior};

    fn settings() -> HookAdmissionSettings {
        HookAdmissionSettings {
            binding_lease_secs: 120,
            dispatch_reserve_micros: crate::money::MicroUsd::ZERO,
            on_coordinator_unreachable: CoordinatorUnreachableBehavior::CarryOn,
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

    /// A spawn the coordinator has never heard of (no run registered)
    /// refuses through the whole wiring, and the reason on stdout names
    /// the run.
    #[test]
    fn an_unregistered_run_refuses_through_the_whole_wiring() {
        let gate = LocalGate::new(ConcurrencyLimits::default());
        let payload = spawn_payload("session-unknown", "tool-1");
        let handled = handle(&payload, &settings(), &gate);
        match handled.answer {
            HookAnswer::Refuse { reason } => {
                assert!(reason.contains("is not registered"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(matches!(
            handled.coordinator,
            Some(crate::admission::Decision::Refused { .. })
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
        let payload = spawn_payload("session-refused", "tool-3");
        let handled = handle(&payload, &settings(), &gate);
        assert!(matches!(handled.answer, HookAnswer::Refuse { .. }));
        assert_eq!(
            gate.withdrawals.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the refusal path must withdraw the dispatch it asked about"
        );
    }

    /// `PostToolUse` never asks the coordinator at all — asking about a
    /// phase `decide` never refuses would reserve a seat this module
    /// has no later chance to release.
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
        fn enforcement(&self) -> crate::admission::Enforcement {
            crate::admission::Enforcement::InProcess
        }
    }

    #[test]
    fn a_post_tool_use_never_asks_the_coordinator() {
        let payload = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "session-x",
            "tool_name": "Agent",
            "tool_use_id": "tool-4",
        })
        .to_string()
        .into_bytes();
        let handled = handle(&payload, &settings(), &PanicsIfAsked);
        assert_eq!(handled.answer, HookAnswer::Silent);
        assert_eq!(handled.coordinator, None);
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
        let payload = spawn_payload("session-j", "tool-j");
        let handled = Handled {
            event: event::parse(&payload),
            coordinator: Some(crate::admission::Decision::Refused {
                code: crate::admission::Refusal::UnknownRun,
                detail: "run r1 is not registered".into(),
            }),
            answer: HookAnswer::Refuse {
                reason: "relais refused this agent: run r1 is not registered. restart the session"
                    .into(),
            },
        };
        let entry = journal_entry(&payload, &handled);
        assert_eq!(entry["decision"], "refuse");
        assert!(entry["reason"]
            .as_str()
            .expect("reason")
            .contains("not registered"));
        assert_eq!(entry["payload"]["session_id"], "session-j");
        assert!(entry["recorded_at"].as_str().is_some());
    }

    #[test]
    fn journal_entry_falls_back_to_a_lossy_string_for_non_json_payloads() {
        let handled = Handled {
            event: HookEvent::NotOurs,
            coordinator: None,
            answer: HookAnswer::Silent,
        };
        let entry = journal_entry(b"not json {{{", &handled);
        assert_eq!(entry["payload"], "not json {{{");
        assert_eq!(entry["decision"], "silent");
        assert!(entry["reason"].is_null());
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
}
