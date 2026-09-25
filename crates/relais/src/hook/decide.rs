//! Deciding what a hook says about one event (SPEC §23).
//!
//! A hook cannot refuse to answer, and it may never say yes on a person's
//! behalf: a hook that green-lit a tool call would override that person's
//! own permission settings, which is not this binary's business. So every
//! case here resolves to
//! exactly one of two things — stay silent, or refuse the tool call and say
//! what was exceeded and what the person can do about it — and the pure
//! function that gets there reads no clock, touches no filesystem and makes
//! no network call: every case can be read and tested without running a
//! session.
//!
//! `decide` takes the event, the machine's own admission settings and
//! whatever the coordinator answered, already resolved — it does not itself
//! contact the coordinator, write a journal, touch the ledger or install
//! anything. It decides, and [`HookAnswer::stdout_payload`] renders the
//! decision into the one shape a hook may speak in.
//!
//! `super::respond::handle` is the caller that wires this up: `relais
//! hook`, run with no flags, asks the coordinator about a spawn, calls
//! [`decide_or_silent`], prints [`HookAnswer::stdout_payload`] when there
//! is one, and journals the firing. This module stays pure regardless —
//! the wiring is what changed, not the decision.
//!
//! `PostToolUseFailure` is deliberately not among the events this decides
//! on: across five probe sessions (`tests/fixtures/hooks/README.md` and
//! `tests/fixtures/hooks-concurrent/README.md`) it has never fired,
//! including one whose tool call genuinely failed and arrived as an
//! ordinary `PostToolUse` carrying the error. Installing a handler for an
//! event that does not arrive would imply a coverage that does not exist.
//! If a later probe run records one, that recording is what should reopen
//! this — until then it is matched explicitly below, alongside every other
//! phase, and resolves to silence like the rest.

use std::time::Duration;

use super::event::{HookEvent, ToolCallPhase};
use crate::admission::{Decision, Refusal};
use crate::policy::{CoordinatorUnreachableBehavior, HookAdmissionSettings};

/// The one shape a hook may speak in. There is deliberately no variant that
/// says yes on a person's behalf — see the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAnswer {
    /// Nothing to say: exit 0, nothing on stdout.
    Silent,
    /// Refuse the tool call. `reason` names what was exceeded and what the
    /// person can do about it: a message a person cannot act on gets
    /// ignored, and this one arrives in the middle of their work.
    Refuse { reason: String },
}

impl HookAnswer {
    /// The stdout payload for this answer: `None` for [`HookAnswer::Silent`]
    /// — literally nothing is printed — and `Some` JSON for
    /// [`HookAnswer::Refuse`]. Pure: it renders a string, it does not print
    /// one; the caller that owns stdout does the actual write.
    ///
    /// The `hookSpecificOutput.permissionDecision` form, not the older
    /// top-level `{"decision":"block"}` one. Both were run against real
    /// Claude Code 2.1.282 before choosing: each blocked the tool call,
    /// and each reached the model identically, as
    /// `PreToolUse:Read hook error: <reason>`. So this is not a
    /// behaviour change — it is the spelling that is not deprecated,
    /// picked because the two are otherwise indistinguishable.
    ///
    /// Worth re-running that comparison rather than trusting this
    /// comment if the refusal ever stops taking effect: a hook whose
    /// deny is ignored exits 0, the tool call proceeds, and nothing
    /// anywhere reports a failure.
    pub fn stdout_payload(&self) -> Option<String> {
        match self {
            HookAnswer::Silent => None,
            HookAnswer::Refuse { reason } => Some(
                serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": reason,
                    }
                })
                .to_string(),
            ),
        }
    }
}

/// Whatever the coordinator answered about a spawn: `None` when it could
/// not be reached at all (a prior communication attempt, entirely outside
/// this module's business), `Some` with its own pure [`Decision`]
/// otherwise.
pub type CoordinatorAnswer = Option<Decision>;

/// Decide what a hook should do about one event, as a pure function of the
/// event, the machine's admission settings, whatever the coordinator
/// answered, and how long `respond::wait_for_a_seat` already waited for
/// it. `waited` is a value handed in, never a clock read here: the
/// sleeping, the clock and the socket all live in `respond` (SPEC §23),
/// and this function stays pure and total.
pub fn decide(
    event: &HookEvent,
    settings: &HookAdmissionSettings,
    coordinator: CoordinatorAnswer,
    waited: Duration,
) -> HookAnswer {
    match event {
        HookEvent::SessionStart(_)
        | HookEvent::SessionEnd(_)
        | HookEvent::SubagentStart(_)
        | HookEvent::SubagentStop(_)
        | HookEvent::NotOurs => HookAnswer::Silent,
        HookEvent::AgentToolCall(call) => match call.phase {
            // Only `Pre` still holds the tool call open long enough to
            // refuse it.
            ToolCallPhase::Pre => decide_spawn(settings, coordinator, waited),
            ToolCallPhase::Post { .. } => HookAnswer::Silent,
            // Never observed to fire (see module doc); matched explicitly
            // rather than folded into a wildcard, so the exclusion is
            // recorded here rather than merely implied.
            ToolCallPhase::PostFailure => HookAnswer::Silent,
        },
    }
}

/// Decide about one spawn (a `PreToolUse` on the Agent tool), given what
/// the coordinator answered and how long this firing already waited for
/// it.
fn decide_spawn(
    settings: &HookAdmissionSettings,
    coordinator: CoordinatorAnswer,
    waited: Duration,
) -> HookAnswer {
    match coordinator {
        None => match settings.on_coordinator_unreachable {
            // Carrying on records the outage elsewhere and stays silent
            // here; refusing declines and says the coordinator is
            // unreachable (SPEC §23).
            CoordinatorUnreachableBehavior::CarryOn => HookAnswer::Silent,
            CoordinatorUnreachableBehavior::Refuse => HookAnswer::Refuse {
                reason: "relais could not reach its coordinator, and this machine is \
                         configured to refuse admission rather than carry on unmanaged. \
                         Retry once the coordinator is reachable (`relais doctor` reports \
                         its status), or set `on_coordinator_unreachable = \"carry_on\"` \
                         under `[admission]` in machine.toml if an unmanaged spawn is \
                         acceptable here."
                    .to_string(),
            },
        },
        // Neither says yes on the person's behalf — see the module doc —
        // they simply have nothing to refuse.
        Some(Decision::Granted | Decision::AlreadyAdmitted) => HookAnswer::Silent,
        // Still queued after `respond::wait_for_a_seat` has already
        // waited up to `settings.queue_behaviour()`'s bound: refused now,
        // not silently. Measured on Claude Code 2.1.282 (SPEC §23): a
        // hook DOES hold its tool call open for as long as it runs, so
        // this is not "cannot wait" — it waited exactly `waited`, and is
        // giving up because a hook cannot wait forever, and the harness's
        // own handler timeout is a hard ceiling this one must answer
        // inside of (`install::settings::derived_pretooluse_timeout`).
        // Staying silent instead would let the agent run anyway AND leave
        // the queue entry counting against the run's cap — the spawn
        // admitted twice over, once in fact and once on paper — so the
        // seat is released by the wait itself before this is ever
        // printed.
        Some(Decision::Queued { position }) => HookAnswer::Refuse {
            reason: format!(
                "relais did not admit this agent: the session is at its limit and this spawn \
                 was queued at position {position}. relais waited {} for a seat before giving \
                 up. Retry when a running agent finishes, or start the work with `relais run`, \
                 which can wait longer.",
                format_waited(waited),
            ),
        },
        Some(Decision::Refused { code, detail }) => HookAnswer::Refuse {
            reason: refusal_message(code, &detail),
        },
    }
}

/// How long this firing waited, rendered the way a person reads it
/// rather than as a raw millisecond count. Whole seconds once the wait
/// crosses one, because nobody needs sub-second precision on a bound that
/// defaults to two seconds; milliseconds below that, because "0 seconds"
/// would understate a real, if short, wait.
fn format_waited(waited: Duration) -> String {
    if waited >= Duration::from_secs(1) {
        format!("{:.1}s", waited.as_secs_f64())
    } else {
        format!("{}ms", waited.as_millis())
    }
}

/// What was exceeded and what the person can do about it, for every way
/// admission can refuse a spawn. A table, not a `_ =>` arm: a new
/// [`Refusal`] variant fails to compile here until this says what it means.
fn refusal_message(code: Refusal, detail: &str) -> String {
    let advice = match code {
        Refusal::UnknownRun => {
            "the coordinator has no record of this run; restart the session that started it"
        }
        Refusal::RunCancelled => "this run was cancelled; nothing further is admitted under it",
        // BOTH files, because the cap in force is the SMALLER of the
        // two: the coordinator takes `min(run limit, machine limit)`
        // (`admission::AdmissionState`), so advice naming only one sends
        // a person to edit a file that may not be the binding
        // constraint — they raise it, are refused again, and have no
        // reason to suspect the other.
        Refusal::DepthExceeded => {
            "the cap in force is the smaller of `max_agent_depth` under `[execution]` in \
             relais.toml and the same key under `[concurrency]` in machine.toml — raise \
             whichever is lower, or nest one level less deep"
        }
        Refusal::RunAgentCap => {
            "the cap in force is the smaller of `max_agents_total` under `[execution]` in \
             relais.toml and `max_agents_per_run` under `[concurrency]` in machine.toml — \
             raise whichever is lower, or start a new run"
        }
        Refusal::BudgetExceeded => {
            "raise `per_run_micros` under `[spending]` in machine.toml, or wait for \
             outstanding reservations to settle"
        }
        // Deliberately no remedy: there is nothing for a person to do.
        // The dispatch id is derived from the tool call
        // (`ids::derive_dispatch_id`), so a person inside a session
        // cannot mint a new one, and advising them to would be advice
        // that cannot be followed. Saying the work already happened is
        // the whole of what is useful here.
        Refusal::AlreadyFinished => {
            "this dispatch already ran and settled, so nothing further is admitted under it"
        }
    };
    format!("relais refused this agent: {detail}. {advice}.")
}

/// [`decide`] wrapped so a panic anywhere inside it cannot take the tool
/// call with it: a hook that dies mid-decision would otherwise leave
/// Claude Code holding a call nobody ever answered. The wrapped call is
/// pure, so unwinding out of it leaves nothing in an inconsistent state to
/// worry about.
pub fn decide_or_silent(
    event: &HookEvent,
    settings: &HookAdmissionSettings,
    coordinator: CoordinatorAnswer,
    waited: Duration,
) -> HookAnswer {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        decide(event, settings, coordinator, waited)
    }))
    .unwrap_or(HookAnswer::Silent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::event::{
        AgentToolCall, SessionEnd, SessionStart, SubagentStart, SubagentStop,
    };
    use crate::ids::{AgentId, AgentType, PromptId, SessionId, ToolUseId};
    use crate::money::MicroUsd;

    /// The rule the whole module exists to keep true: the production code
    /// in this file never spells out the word for saying yes on a
    /// person's behalf. Greps this file's own source — everything up to
    /// its `#[cfg(test)]` boundary, so the test module (which has to name
    /// the very word it is checking for) does not trip its own check —
    /// rather than trusting a reviewer to keep noticing on every future
    /// edit.
    #[test]
    fn production_code_never_spells_out_the_forbidden_word() {
        let source = include_str!("decide.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .map(|(before, _)| before)
            .expect("this file has a #[cfg(test)] module");
        assert!(
            !production.to_ascii_lowercase().contains("allow"),
            "production code in this module must never write `allow`"
        );
    }

    // Zero: `decide` itself never waits (`respond::wait_for_a_seat` does,
    // and has its own tests) — its `Queued` cases here are exercised as
    // an already-resolved coordinator answer, with `waited` passed in
    // directly, so nothing in this file's tests needs a real wait.
    fn carry_on() -> HookAdmissionSettings {
        HookAdmissionSettings {
            binding_lease_secs: 120,
            dispatch_reserve_micros: MicroUsd::ZERO,
            on_coordinator_unreachable: CoordinatorUnreachableBehavior::CarryOn,
            queue_wait_secs: 0,
        }
    }

    fn refuse_on_unreachable() -> HookAdmissionSettings {
        HookAdmissionSettings {
            on_coordinator_unreachable: CoordinatorUnreachableBehavior::Refuse,
            ..carry_on()
        }
    }

    fn spawn(phase: ToolCallPhase) -> HookEvent {
        HookEvent::AgentToolCall(AgentToolCall {
            session_id: SessionId::new("s1"),
            tool_use_id: ToolUseId::new("t1"),
            phase,
            caller_agent_id: None,
            prompt_id: Some(PromptId::new("p1")),
        })
    }

    fn is_silent(answer: &HookAnswer) -> bool {
        matches!(answer, HookAnswer::Silent)
    }

    /// Every event kind that is not a `PreToolUse` spawn: none of them is
    /// ever refused, whatever the coordinator says.
    #[test]
    fn every_non_pre_spawn_event_is_silent() {
        let events: Vec<(&str, HookEvent)> = vec![
            (
                "SessionStart",
                HookEvent::SessionStart(SessionStart {
                    session_id: SessionId::new("s1"),
                }),
            ),
            (
                "SessionEnd",
                HookEvent::SessionEnd(SessionEnd {
                    session_id: SessionId::new("s1"),
                }),
            ),
            (
                "SubagentStart",
                HookEvent::SubagentStart(SubagentStart {
                    session_id: SessionId::new("s1"),
                    agent_id: AgentId::new("a1"),
                    agent_type: AgentType::new("general-purpose"),
                    prompt_id: None,
                }),
            ),
            (
                "SubagentStop",
                HookEvent::SubagentStop(SubagentStop {
                    session_id: SessionId::new("s1"),
                    agent_id: AgentId::new("a1"),
                    agent_type: AgentType::new("general-purpose"),
                    prompt_id: None,
                }),
            ),
            ("NotOurs", HookEvent::NotOurs),
            (
                "PostToolUse",
                spawn(ToolCallPhase::Post {
                    launched_agent: Some(AgentId::new("a1")),
                }),
            ),
            ("PostToolUseFailure", spawn(ToolCallPhase::PostFailure)),
        ];
        // Every stance against every coordinator answer. What this pins
        // is structural: a non-`Pre` event reaches neither the
        // coordinator's answer nor the outage stance, because `decide`
        // settles it on the event alone. So no combination here can
        // refuse a call that has already returned — and refusing one
        // would report a failure for work that had succeeded.
        //
        // Verified to bite by routing `Post` through `decide_spawn`,
        // which fails this naming the event, the stance and the answer.
        // Note what does NOT bite: making `Granted` refuse leaves this
        // green, because non-`Pre` events never consult it. The
        // invariant is the routing, not the arms.
        for (name, event) in events {
            for settings in [carry_on(), refuse_on_unreachable()] {
                for coordinator in [
                    None,
                    Some(Decision::Granted),
                    Some(Decision::AlreadyAdmitted),
                    Some(Decision::Refused {
                        code: Refusal::RunAgentCap,
                        detail: "the run is at its agent cap".to_string(),
                    }),
                ] {
                    let answer = decide(&event, &settings, coordinator.clone(), Duration::ZERO);
                    assert!(
                        is_silent(&answer),
                        "{name} under {:?} with {coordinator:?}: {answer:?}",
                        settings.on_coordinator_unreachable
                    );
                }
            }
        }
    }

    /// The coordinator's answer to a `PreToolUse` spawn, table-driven over
    /// every case [`Decision`] can reach: never an `Allow`, a refusal only
    /// for `Refused`, and the unreachable case handled by machine settings.
    #[test]
    fn a_spawns_decision_is_table_driven_over_every_coordinator_answer() {
        struct Case {
            name: &'static str,
            settings: HookAdmissionSettings,
            coordinator: CoordinatorAnswer,
            waited: Duration,
            expect_refusal: bool,
        }
        let cases = vec![
            Case {
                name: "unreachable, carry on",
                settings: carry_on(),
                coordinator: None,
                waited: Duration::ZERO,
                expect_refusal: false,
            },
            Case {
                name: "unreachable, refuse",
                settings: refuse_on_unreachable(),
                coordinator: None,
                waited: Duration::ZERO,
                expect_refusal: true,
            },
            Case {
                name: "granted",
                settings: carry_on(),
                coordinator: Some(Decision::Granted),
                waited: Duration::ZERO,
                expect_refusal: false,
            },
            Case {
                name: "already admitted",
                settings: carry_on(),
                coordinator: Some(Decision::AlreadyAdmitted),
                waited: Duration::ZERO,
                expect_refusal: false,
            },
            // Refused, not silent, once `respond::wait_for_a_seat` has
            // already waited out its bound: staying quiet would run the
            // agent AND leave its queue entry counting against the cap.
            Case {
                name: "queued",
                settings: carry_on(),
                coordinator: Some(Decision::Queued { position: 3 }),
                waited: Duration::from_millis(1_800),
                expect_refusal: true,
            },
            Case {
                name: "refused: unknown run",
                settings: carry_on(),
                coordinator: Some(Decision::Refused {
                    code: Refusal::UnknownRun,
                    detail: "run r1 is not registered".into(),
                }),
                waited: Duration::ZERO,
                expect_refusal: true,
            },
            Case {
                name: "refused: run cancelled",
                settings: carry_on(),
                coordinator: Some(Decision::Refused {
                    code: Refusal::RunCancelled,
                    detail: "run r1 was cancelled".into(),
                }),
                waited: Duration::ZERO,
                expect_refusal: true,
            },
            Case {
                name: "refused: depth exceeded",
                settings: carry_on(),
                coordinator: Some(Decision::Refused {
                    code: Refusal::DepthExceeded,
                    detail: "depth 4 exceeds the effective maximum 3".into(),
                }),
                waited: Duration::ZERO,
                expect_refusal: true,
            },
            Case {
                name: "refused: run agent cap",
                settings: carry_on(),
                coordinator: Some(Decision::Refused {
                    code: Refusal::RunAgentCap,
                    detail: "run r1 has used its aggregate agent cap (24)".into(),
                }),
                waited: Duration::ZERO,
                expect_refusal: true,
            },
            Case {
                name: "refused: budget exceeded",
                settings: carry_on(),
                coordinator: Some(Decision::Refused {
                    code: Refusal::BudgetExceeded,
                    detail: "reserving 500 on top of 900 committed exceeds the run budget 1000"
                        .into(),
                }),
                waited: Duration::ZERO,
                expect_refusal: true,
            },
            Case {
                name: "refused: already finished",
                settings: carry_on(),
                coordinator: Some(Decision::Refused {
                    code: Refusal::AlreadyFinished,
                    detail: "dispatch d1 already ran and settled".into(),
                }),
                waited: Duration::ZERO,
                expect_refusal: true,
            },
        ];

        for case in cases {
            let event = spawn(ToolCallPhase::Pre);
            let answer = decide(
                &event,
                &case.settings,
                case.coordinator.clone(),
                case.waited,
            );
            match (&answer, case.expect_refusal) {
                (HookAnswer::Silent, false) => {}
                (HookAnswer::Refuse { reason }, true) => {
                    assert!(!reason.is_empty(), "{}: empty reason", case.name);
                    // What was exceeded (the detail) and what to do about
                    // it (the advice) both have to be in the message a
                    // person actually sees.
                    if let Some(Decision::Refused { detail, .. }) = &case.coordinator {
                        assert!(
                            reason.contains(detail.as_str()),
                            "{}: reason `{reason}` drops the detail",
                            case.name
                        );
                    }
                    // The queued case is the one the false claim used to
                    // live in: the message must say how long this firing
                    // actually waited, and never say a hook cannot hold
                    // its call open — it just did, for `case.waited`.
                    if matches!(&case.coordinator, Some(Decision::Queued { .. })) {
                        assert!(reason.contains("1.8s"), "{}: {reason}", case.name);
                        assert!(
                            !reason.contains("cannot hold a tool call open"),
                            "{}: {reason}",
                            case.name
                        );
                    }
                }
                _ => panic!(
                    "{}: expected refusal={}, got {answer:?}",
                    case.name, case.expect_refusal
                ),
            }
        }
    }

    #[test]
    fn silent_has_no_stdout_payload() {
        assert_eq!(HookAnswer::Silent.stdout_payload(), None);
    }

    #[test]
    fn a_refusal_renders_reason_and_never_the_word_allow_on_stdout() {
        let answer = HookAnswer::Refuse {
            reason: "depth 4 exceeds the effective maximum 3. raise max_agent_depth.".into(),
        };
        let payload = answer.stdout_payload().expect("refusal renders a payload");
        assert!(payload.contains("\"permissionDecision\":\"deny\""));
        assert!(payload.contains("depth 4 exceeds"));
        assert!(!payload.to_ascii_lowercase().contains("allow"));
    }

    /// `decide_or_silent` is the boundary a caller actually uses: for every
    /// case in the table above it agrees with `decide` exactly, because
    /// nothing in `decide` panics — the wrapper's `catch_unwind` is there
    /// for a future change that introduces one, not because this one does.
    #[test]
    fn decide_or_silent_agrees_with_decide_when_nothing_panics() {
        let event = spawn(ToolCallPhase::Pre);
        for coordinator in [
            None,
            Some(Decision::Granted),
            Some(Decision::Refused {
                code: Refusal::DepthExceeded,
                detail: "depth 4 exceeds the effective maximum 3".into(),
            }),
        ] {
            assert_eq!(
                decide(
                    &event,
                    &carry_on(),
                    coordinator.clone(),
                    Duration::from_millis(1_234)
                ),
                decide_or_silent(
                    &event,
                    &carry_on(),
                    coordinator,
                    Duration::from_millis(1_234)
                ),
            );
        }
    }
}
