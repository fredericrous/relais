//! Turn one Claude Code hook payload into a typed [`HookEvent`].
//!
//! Parsing only: this module decides nothing, admits nothing and
//! records nothing — it establishes what a payload IS, so the packages
//! that act on it argue about policy rather than about JSON. The nine
//! payloads under `crates/relais/tests/fixtures/hooks` are transcribed
//! from a real session (see that directory's `README.md`); this module
//! is written against what they showed, not against invented shapes.

use serde::Deserialize;

use crate::ids::{AgentId, AgentType, PromptId, SessionId, ToolUseId};

/// A payload past this size parses into [`HookEvent::NotOurs`] rather
/// than being attempted at all. A hook cannot refuse to answer, so an
/// oversized payload is a thing relais declines to act on, never a
/// crash and never an error surfaced to the session.
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;

/// The tool name Claude Code sends for a spawn today.
const AGENT_TOOL_NAME: &str = "Agent";

/// The name Claude Code used for the same tool before it was renamed. A
/// contract written against either name behaves the same way.
const LEGACY_AGENT_TOOL_NAME: &str = "Task";

/// A hook payload, typed. Every payload lands in exactly one variant —
/// never an error, never a panic — because a hook cannot refuse to
/// answer: the tool call or lifecycle transition it describes is
/// blocked until the handler responds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookEvent {
    SessionStart(SessionStart),
    SessionEnd(SessionEnd),
    SubagentStart(SubagentStart),
    SubagentStop(SubagentStop),
    /// A `PreToolUse`/`PostToolUse`/`PostToolUseFailure` event whose
    /// tool is the Agent tool (or its legacy name, `Task`) — the only
    /// tool relais acts on.
    AgentToolCall(AgentToolCall),
    /// Not something relais acts on. One case covers two different
    /// reasons, deliberately: a tool event for a tool other than the
    /// Agent tool (`0001-PreToolUse.json`, a `Read`, lands here), and a
    /// payload that is not JSON, is not an object, names no event,
    /// names an event this binary does not know, or exceeds
    /// [`MAX_EVENT_BYTES`]. A hook cannot refuse to answer, and an
    /// unparseable payload is a thing relais declines to act on, never
    /// a crash and never an error surfaced to the session — so both
    /// reasons get the same answer rather than one of them becoming a
    /// malformed attempt at a richer variant.
    NotOurs,
}

/// The session lifecycle beginning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStart {
    pub session_id: SessionId,
}

/// The session lifecycle ending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEnd {
    pub session_id: SessionId,
}

/// A subagent beginning. Carries no `tool_use_id` — nothing in this
/// payload joins the tool call that admitted the agent to the agent
/// that then ran; that join needs `agent_type`, arrival order and
/// `prompt_id` from elsewhere, which is not this module's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentStart {
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub agent_type: AgentType,
    pub prompt_id: Option<PromptId>,
}

/// A subagent ending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentStop {
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub agent_type: AgentType,
    pub prompt_id: Option<PromptId>,
}

/// Which of the three tool-scoped events a payload was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallPhase {
    Pre,
    Post,
    PostFailure,
}

/// A tool-scoped event whose tool is the Agent tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolCall {
    pub session_id: SessionId,
    pub tool_use_id: ToolUseId,
    pub phase: ToolCallPhase,
    /// The CALLER's agent — the agent that made this tool call, not the
    /// agent it is calling. Genuinely absent at the top level when the
    /// caller is the main session: `0002-PreToolUse.json`, the spawn
    /// itself, carries no `agent_id`. Present when the caller is itself
    /// a subagent: `0004-PreToolUse.json`, a call the subagent made,
    /// carries one. See `caller_agent_id_is_absent_on_the_spawn_and_present_on_a_subagents_call`.
    pub caller_agent_id: Option<AgentId>,
    pub prompt_id: Option<PromptId>,
}

/// The fields this module reads to classify a payload — deliberately
/// not `#[serde(deny_unknown_fields)]`, unlike this crate's usual habit.
/// The recorded payloads carry `cwd`, `effort`, `permission_mode`,
/// `transcript_path`, `background_tasks`, `session_crons`,
/// `stop_hook_active` and more this module has no use for, and a future
/// Claude Code release will add others it has not carried yet.
/// Rejecting fields this module does not model would turn every harness
/// release into an outage.
#[derive(Debug, Clone, Deserialize)]
struct Envelope {
    hook_event_name: Option<String>,
    session_id: Option<SessionId>,
    prompt_id: Option<PromptId>,
    agent_id: Option<AgentId>,
    agent_type: Option<AgentType>,
    tool_name: Option<String>,
    tool_use_id: Option<ToolUseId>,
}

/// Parse one hook payload into a typed event. Never fails outward — see
/// [`HookEvent::NotOurs`] for what happens to a payload this module
/// cannot make sense of.
pub fn parse(bytes: &[u8]) -> HookEvent {
    if bytes.len() > MAX_EVENT_BYTES {
        return HookEvent::NotOurs;
    }
    // A payload that fails to parse this far is not JSON, not an
    // object, or shaped in a way none of the fields above can hold —
    // every one of those is a payload relais declines to act on, not a
    // fault to surface.
    let Ok(envelope) = serde_json::from_slice::<Envelope>(bytes) else {
        return HookEvent::NotOurs;
    };
    classify(envelope)
}

fn classify(envelope: Envelope) -> HookEvent {
    let Some(event_name) = envelope.hook_event_name.as_deref() else {
        return HookEvent::NotOurs;
    };
    match event_name {
        "SessionStart" => match envelope.session_id {
            Some(session_id) => HookEvent::SessionStart(SessionStart { session_id }),
            None => HookEvent::NotOurs,
        },
        "SessionEnd" => match envelope.session_id {
            Some(session_id) => HookEvent::SessionEnd(SessionEnd { session_id }),
            None => HookEvent::NotOurs,
        },
        "SubagentStart" => {
            classify_subagent(envelope, |session_id, agent_id, agent_type, prompt_id| {
                HookEvent::SubagentStart(SubagentStart {
                    session_id,
                    agent_id,
                    agent_type,
                    prompt_id,
                })
            })
        }
        "SubagentStop" => {
            classify_subagent(envelope, |session_id, agent_id, agent_type, prompt_id| {
                HookEvent::SubagentStop(SubagentStop {
                    session_id,
                    agent_id,
                    agent_type,
                    prompt_id,
                })
            })
        }
        "PreToolUse" => classify_tool_call(ToolCallPhase::Pre, envelope),
        "PostToolUse" => classify_tool_call(ToolCallPhase::Post, envelope),
        "PostToolUseFailure" => classify_tool_call(ToolCallPhase::PostFailure, envelope),
        _ => HookEvent::NotOurs,
    }
}

fn classify_subagent(
    envelope: Envelope,
    build: impl FnOnce(SessionId, AgentId, AgentType, Option<PromptId>) -> HookEvent,
) -> HookEvent {
    match (envelope.session_id, envelope.agent_id, envelope.agent_type) {
        (Some(session_id), Some(agent_id), Some(agent_type)) => {
            build(session_id, agent_id, agent_type, envelope.prompt_id)
        }
        _ => HookEvent::NotOurs,
    }
}

fn classify_tool_call(phase: ToolCallPhase, envelope: Envelope) -> HookEvent {
    let is_agent_tool = matches!(
        envelope.tool_name.as_deref(),
        Some(AGENT_TOOL_NAME) | Some(LEGACY_AGENT_TOOL_NAME)
    );
    if !is_agent_tool {
        return HookEvent::NotOurs;
    }
    match (envelope.session_id, envelope.tool_use_id) {
        (Some(session_id), Some(tool_use_id)) => HookEvent::AgentToolCall(AgentToolCall {
            session_id,
            tool_use_id,
            phase,
            caller_agent_id: envelope.agent_id,
            prompt_id: envelope.prompt_id,
        }),
        _ => HookEvent::NotOurs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/hooks")
            .join(name);
        fs::read(&path).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
    }

    #[test]
    fn a_session_start_payload_parses() {
        let event = parse(br#"{"hook_event_name":"SessionStart","session_id":"s1"}"#);
        assert_eq!(
            event,
            HookEvent::SessionStart(SessionStart {
                session_id: SessionId::new("s1")
            })
        );
    }

    #[test]
    fn a_session_end_payload_parses() {
        let event = parse(br#"{"hook_event_name":"SessionEnd","session_id":"s1"}"#);
        assert_eq!(
            event,
            HookEvent::SessionEnd(SessionEnd {
                session_id: SessionId::new("s1")
            })
        );
    }

    #[test]
    fn a_subagent_start_payload_parses() {
        let event = parse(
            br#"{"hook_event_name":"SubagentStart","session_id":"s1","agent_id":"a1","agent_type":"general-purpose","prompt_id":"p1"}"#,
        );
        assert_eq!(
            event,
            HookEvent::SubagentStart(SubagentStart {
                session_id: SessionId::new("s1"),
                agent_id: AgentId::new("a1"),
                agent_type: AgentType::new("general-purpose"),
                prompt_id: Some(PromptId::new("p1")),
            })
        );
    }

    #[test]
    fn a_subagent_stop_payload_parses() {
        let event = parse(
            br#"{"hook_event_name":"SubagentStop","session_id":"s1","agent_id":"a1","agent_type":"general-purpose"}"#,
        );
        assert_eq!(
            event,
            HookEvent::SubagentStop(SubagentStop {
                session_id: SessionId::new("s1"),
                agent_id: AgentId::new("a1"),
                agent_type: AgentType::new("general-purpose"),
                prompt_id: None,
            })
        );
    }

    #[test]
    fn an_agent_tool_call_parses_for_each_phase() {
        for (name, phase) in [
            ("PreToolUse", ToolCallPhase::Pre),
            ("PostToolUse", ToolCallPhase::Post),
            ("PostToolUseFailure", ToolCallPhase::PostFailure),
        ] {
            let payload = format!(
                r#"{{"hook_event_name":"{name}","session_id":"s1","tool_name":"Agent","tool_use_id":"t1"}}"#
            );
            let event = parse(payload.as_bytes());
            assert_eq!(
                event,
                HookEvent::AgentToolCall(AgentToolCall {
                    session_id: SessionId::new("s1"),
                    tool_use_id: ToolUseId::new("t1"),
                    phase,
                    caller_agent_id: None,
                    prompt_id: None,
                }),
                "phase {name}"
            );
        }
    }

    /// The legacy tool name predates the `Agent` rename. A contract
    /// written against either name behaves the same.
    #[test]
    fn the_legacy_task_tool_name_is_accepted_alongside_agent() {
        let event = parse(
            br#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Task","tool_use_id":"t1"}"#,
        );
        assert!(matches!(event, HookEvent::AgentToolCall(_)));
    }

    #[test]
    fn a_non_agent_tool_call_is_not_ours() {
        let event = parse(
            br#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Read","tool_use_id":"t1"}"#,
        );
        assert_eq!(event, HookEvent::NotOurs);
    }

    #[test]
    fn invalid_json_is_not_ours() {
        assert_eq!(parse(b"not json at all {{{"), HookEvent::NotOurs);
    }

    #[test]
    fn a_json_array_is_not_ours() {
        assert_eq!(parse(b"[1,2,3]"), HookEvent::NotOurs);
    }

    #[test]
    fn a_payload_naming_no_event_is_not_ours() {
        assert_eq!(parse(br#"{"session_id":"s1"}"#), HookEvent::NotOurs);
    }

    #[test]
    fn a_payload_naming_an_unknown_event_is_not_ours() {
        assert_eq!(
            parse(br#"{"hook_event_name":"SomethingNew","session_id":"s1"}"#),
            HookEvent::NotOurs
        );
    }

    #[test]
    fn a_payload_past_the_cap_is_not_ours() {
        let mut big = br#"{"hook_event_name":"SessionStart","session_id":""#.to_vec();
        big.extend(std::iter::repeat_n(b'a', MAX_EVENT_BYTES + 1));
        big.extend_from_slice(br#""}"#);
        assert_eq!(parse(&big), HookEvent::NotOurs);
    }

    #[test]
    fn a_payload_with_fields_this_crate_does_not_model_still_parses() {
        let event = parse(
            br#"{"hook_event_name":"SessionStart","session_id":"s1","cwd":"/repo","effort":{"level":"medium"},"permission_mode":"default","transcript_path":"/t.jsonl","background_tasks":[],"session_crons":[],"stop_hook_active":false,"a_field_from_the_future":42}"#,
        );
        assert_eq!(
            event,
            HookEvent::SessionStart(SessionStart {
                session_id: SessionId::new("s1")
            })
        );
    }

    /// The measurement the whole module is pinned to: `0002` is the
    /// spawn itself, made by the main session, and carries no
    /// `agent_id`; `0004` is a call the subagent made, and carries one.
    /// Read through the same [`Envelope`] `parse` uses internally, so
    /// this is a test of the measurement, not of a guess about it.
    #[test]
    fn caller_agent_id_is_absent_on_the_spawn_and_present_on_a_subagents_call() {
        let spawn: Envelope =
            serde_json::from_slice(&fixture("0002-PreToolUse.json")).expect("parse 0002");
        assert_eq!(spawn.agent_id, None);
        assert_eq!(spawn.tool_name.as_deref(), Some("Agent"));

        let by_subagent: Envelope =
            serde_json::from_slice(&fixture("0004-PreToolUse.json")).expect("parse 0004");
        assert_eq!(by_subagent.agent_id, Some(AgentId::new("agent-01")));
    }

    /// `0001-PreToolUse.json` is a `Read`, made by the main session, not
    /// a malformed attempt at an agent event.
    #[test]
    fn fixture_0001_a_read_tool_call_is_not_ours() {
        assert_eq!(parse(&fixture("0001-PreToolUse.json")), HookEvent::NotOurs);
    }

    #[test]
    fn fixture_0002_the_spawn_is_an_agent_tool_call_with_no_caller_agent() {
        let event = parse(&fixture("0002-PreToolUse.json"));
        let HookEvent::AgentToolCall(call) = event else {
            panic!("expected an agent tool call, got {event:?}");
        };
        assert_eq!(call.caller_agent_id, None);
        assert_eq!(call.phase, ToolCallPhase::Pre);
    }

    #[test]
    fn fixture_0003_a_subagent_start_carries_its_agent_and_type() {
        let event = parse(&fixture("0003-SubagentStart.json"));
        assert_eq!(
            event,
            HookEvent::SubagentStart(SubagentStart {
                session_id: SessionId::new("session-0000"),
                agent_id: AgentId::new("agent-01"),
                agent_type: AgentType::new("general-purpose"),
                prompt_id: Some(PromptId::new("prompt-0000")),
            })
        );
    }

    // The fixture walk lives in `tests/hook_event_fixtures.rs`, not
    // here. Two copies of one verification drift apart, and the copy
    // that drifts is the one nobody looks at; the integration test is
    // where a reader goes looking for "what do the real payloads do".
    // The tests above use inline payloads ON PURPOSE — they pin the
    // shapes a recording does NOT contain (malformed input, an unknown
    // event, an oversized body), which no fixture can demonstrate.
}
