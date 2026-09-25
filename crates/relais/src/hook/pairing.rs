//! Pairing a spawn to the agent it started (SPEC §23).
//!
//! Nothing in a single hook payload joins the tool call that admitted an
//! agent to the agent that then ran: the spawn (`PreToolUse` on the Agent
//! tool) carries a `tool_use_id` and no `agent_id`, and the agent's own
//! `SubagentStart` carries an `agent_id` and no `tool_use_id`. The join has
//! to be made from arrival order instead, and how sure that join is has to
//! travel with the pairing it produces rather than living in a comment.
//! This module decides nothing about admission and reads nothing off disk
//! — it takes an already-typed stream of [`HookEvent`]s and says which
//! spawn each start belongs to.

use crate::admission::Provenance;
use crate::ids::{AgentId, ToolUseId};

use super::event::{HookEvent, ToolCallPhase};

/// One spawn matched to the agent it started, and how that match was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    pub tool_use_id: ToolUseId,
    pub agent_id: AgentId,
    pub provenance: Provenance,
}

/// Pair every spawn in a stream of hook events, in arrival order, with the
/// `SubagentStart` that answers it.
///
/// A single spawn awaiting a start is paired with the very next start to
/// arrive: known, because `tests/fixtures/hooks-concurrent` recorded five
/// overlapping agents whose `PreToolUse:Agent` was, every time, immediately
/// followed by its own `SubagentStart` — five for five, never interleaved.
/// When a start arrives with more than one spawn still waiting, the oldest
/// is matched instead; no recorded session has ever produced that case (the
/// harness stages spawns about 30ms apart), so that pairing is inferred
/// rather than known, and says what it was inferred from.
pub fn pair(events: &[HookEvent]) -> Vec<Pairing> {
    let mut waiting: Vec<ToolUseId> = Vec::new();
    let mut pairings = Vec::new();
    for event in events {
        match event {
            HookEvent::AgentToolCall(call) if call.phase == ToolCallPhase::Pre => {
                waiting.push(call.tool_use_id.clone());
            }
            HookEvent::SubagentStart(start) => {
                let Some(tool_use_id) = (!waiting.is_empty()).then(|| waiting.remove(0)) else {
                    continue;
                };
                let provenance = if waiting.is_empty() {
                    Provenance::Known
                } else {
                    Provenance::Inferred {
                        basis: format!(
                            "{} more spawn(s) were still awaiting a start when this one \
                             arrived; the oldest was matched (no recorded session has \
                             produced this case)",
                            waiting.len()
                        ),
                    }
                };
                pairings.push(Pairing {
                    tool_use_id,
                    agent_id: start.agent_id.clone(),
                    provenance,
                });
            }
            // The spawn's own result, arriving while it is STILL waiting
            // for a start, is evidence that no agent will come: the
            // recorded sessions show `SubagentStop` preceding its own
            // `PostToolUse` every time (see
            // `tests/fixtures/hooks-concurrent/README.md`), so a start
            // that was going to happen has already happened by now. A
            // denied or failed spawn is the case — and
            // `PostToolUseFailure` has never fired in any probe session,
            // so this result is all the notice there is.
            //
            // Dropping it matters more than it looks. Left in the queue
            // it is matched to the NEXT agent to start, and then every
            // later pairing in the session is off by one — each flagged
            // inferred, and each wrong.
            HookEvent::AgentToolCall(call) => {
                waiting.retain(|waiting_id| waiting_id != &call.tool_use_id);
            }
            HookEvent::SessionStart(_)
            | HookEvent::SessionEnd(_)
            | HookEvent::SubagentStop(_)
            | HookEvent::NotOurs => {}
        }
    }
    pairings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::event::parse;
    use std::fs;
    use std::path::Path;

    /// Every payload under `hooks-concurrent`, parsed in file order — the
    /// filenames are zero-padded arrival indices, so lexical order is
    /// arrival order (see that directory's `README.md`).
    fn concurrent_session() -> Vec<HookEvent> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks-concurrent");
        let mut names: Vec<String> = fs::read_dir(&dir)
            .expect("read hooks-concurrent fixtures")
            // A per-entry read error here would mean the directory
            // changed mid-scan; skipping it is a smaller test fixture,
            // not a hidden failure of the thing under test.
            .filter_map(Result::ok)
            // Every fixture name here is ASCII, written by this repo; a
            // non-UTF-8 entry is not one of them and is skipped rather
            // than panicking the test.
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.ends_with(".json"))
            .collect();
        names.sort();
        names
            .into_iter()
            .map(|name| {
                let bytes = fs::read(dir.join(&name)).unwrap_or_else(|e| panic!("{name}: {e}"));
                parse(&bytes)
            })
            .collect()
    }

    /// The case the fixture actually shows: five spawns, five starts, each
    /// `PreToolUse:Agent` immediately followed by its own `SubagentStart`.
    /// Every pairing is known, because at no point was more than one spawn
    /// waiting.
    #[test]
    fn five_overlapping_spawns_pair_known_from_the_recorded_session() {
        let events = concurrent_session();
        let pairings = pair(&events);
        assert_eq!(pairings.len(), 5, "{pairings:?}");
        for (index, pairing) in pairings.iter().enumerate() {
            let n = index + 1;
            assert_eq!(pairing.tool_use_id, ToolUseId::new(format!("toolu-{n:02}")));
            assert_eq!(pairing.agent_id, AgentId::new(format!("agent-{n:02}")));
            assert_eq!(
                pairing.provenance,
                Provenance::Known,
                "pairing {n}: {pairing:?}"
            );
        }
    }

    /// The case no recording has ever produced: two spawns arrive before
    /// either one's start. Built from an invented sequence rather than a
    /// A spawn whose agent never starts must leave the queue when its
    /// own result arrives, or it is matched to the NEXT agent and every
    /// later pairing in the session is off by one — each flagged
    /// inferred, and each wrong.
    ///
    /// Invented, like the test below: no recording shows a spawn that
    /// produces no agent. The ordering it relies on IS recorded, though
    /// — `SubagentStop` precedes its own `PostToolUse` in every
    /// concurrent session — so a result arriving while its spawn still
    /// waits means the start is never coming.
    #[test]
    fn a_spawn_that_never_starts_does_not_steal_the_next_agent() {
        use crate::hook::event::{AgentToolCall, SubagentStart};
        use crate::ids::{AgentType, SessionId};

        let session_id = SessionId::new("session-invented");
        let call = |tool_use: &str, phase| {
            HookEvent::AgentToolCall(AgentToolCall {
                session_id: session_id.clone(),
                tool_use_id: ToolUseId::new(tool_use),
                phase,
                caller_agent_id: None,
                prompt_id: None,
            })
        };
        let events = vec![
            // Denied, or failed: a spawn that returns without ever
            // producing an agent.
            call("toolu-dead", ToolCallPhase::Pre),
            call(
                "toolu-dead",
                ToolCallPhase::Post {
                    launched_agent: None,
                },
            ),
            // A real spawn, which must get the agent that follows it.
            call("toolu-live", ToolCallPhase::Pre),
            HookEvent::SubagentStart(SubagentStart {
                session_id: session_id.clone(),
                agent_id: AgentId::new("agent-live"),
                agent_type: AgentType::new("general-purpose"),
                prompt_id: None,
            }),
        ];

        let pairings = pair(&events);
        assert_eq!(pairings.len(), 1, "{pairings:?}");
        assert_eq!(pairings[0].tool_use_id, ToolUseId::new("toolu-live"));
        assert_eq!(pairings[0].agent_id, AgentId::new("agent-live"));
        assert_eq!(
            pairings[0].provenance,
            Provenance::Known,
            "nothing else was waiting, so this pairing is observed rather than guessed"
        );
    }

    /// fixture, because — per the README — five probe sessions never
    /// showed it. The pairing that results is inferred, and says what it
    /// was inferred from, rather than being reported as certain.
    #[test]
    fn two_spawns_awaiting_one_start_pair_as_inferred() {
        use crate::hook::event::{AgentToolCall, SubagentStart};
        use crate::ids::{AgentType, SessionId};

        let session_id = SessionId::new("session-invented");
        let events = vec![
            HookEvent::AgentToolCall(AgentToolCall {
                session_id: session_id.clone(),
                tool_use_id: ToolUseId::new("toolu-a"),
                phase: ToolCallPhase::Pre,
                caller_agent_id: None,
                prompt_id: None,
            }),
            HookEvent::AgentToolCall(AgentToolCall {
                session_id: session_id.clone(),
                tool_use_id: ToolUseId::new("toolu-b"),
                phase: ToolCallPhase::Pre,
                caller_agent_id: None,
                prompt_id: None,
            }),
            HookEvent::SubagentStart(SubagentStart {
                session_id,
                agent_id: AgentId::new("agent-a"),
                agent_type: AgentType::new("general-purpose"),
                prompt_id: None,
            }),
        ];

        let pairings = pair(&events);
        assert_eq!(pairings.len(), 1, "{pairings:?}");
        let pairing = &pairings[0];
        assert_eq!(pairing.tool_use_id, ToolUseId::new("toolu-a"));
        assert_eq!(pairing.agent_id, AgentId::new("agent-a"));
        assert!(
            matches!(&pairing.provenance, Provenance::Inferred { basis } if !basis.is_empty()),
            "expected an inferred pairing with a basis, got {:?}",
            pairing.provenance
        );
    }
}
