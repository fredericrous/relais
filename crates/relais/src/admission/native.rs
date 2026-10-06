//! The coordinator's record of native dispatches (SPEC §23).
//!
//! A native dispatch is an ordinary dispatch the runner already admitted
//! under its run, plus one record here, keyed by dispatch id, that follows
//! the subagent the parent session spawns (or continues) on relais's
//! behalf: asked for, claimed by the hook, given its tree, bound to its
//! agent, stopped. The record decides nothing about money or seats: the
//! dispatch settles and releases as any other, when the runner says so,
//! and the record goes with it.
//!
//! Every operation answers with an outcome enum, one variant per way it
//! can end, so the hook (and N2's backend) cannot ignore a refusal.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::{AdmissionState, BindOutcome, Provenance};
use crate::native::{find_marker, Marker};

/// What relais asked the parent session to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub enum NativeAsk {
    /// A fresh subagent, in the tree relais prepared at `worktree`.
    Spawn {
        subagent_type: String,
        model: String,
        prompt: String,
        worktree: PathBuf,
    },
    /// Another turn for a subagent that already ran.
    Continue { agent_id: String, message: String },
}

impl NativeAsk {
    /// The text the run marker must be in: the prompt of a spawn, the
    /// message of a continuation.
    fn marked_text(&self) -> &str {
        match self {
            Self::Spawn { prompt, .. } => prompt,
            Self::Continue { message, .. } => message,
        }
    }
}

/// Where a native dispatch is, from the request to the agent's end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NativeState {
    /// Registered; no call has claimed it.
    Requested { ask: NativeAsk },
    /// A marked call was told to run it. A spawn waits for its tree.
    Claimed,
    /// A spawn's tree was handed out under this name.
    TreeGiven { name: String },
    /// The agent runs under this dispatch's lease.
    Bound { agent_id: String },
    /// The agent ended. The dispatch is NOT settled: the runner settles it
    /// with the real spend.
    Stopped {
        agent_id: String,
        transcript_path: Option<PathBuf>,
        last_assistant_message: Option<String>,
    },
    /// The dispatch cannot run as asked; `reason` is a stable label.
    Failed { reason: String },
}

/// Why a native dispatch failed, as `NativeState::Failed` records it.
const TREE_MISMATCH: &str = "native_tree_mismatch";
const BIND_REFUSED: &str = "native_bind_refused";

#[derive(Debug, Clone)]
struct NativeRecord {
    session_id: String,
    state: NativeState,
    /// A spawn's prepared tree, kept after the claim so a `WorktreeCreate`
    /// can be answered with it.
    worktree: Option<PathBuf>,
    /// Which claim this was, counted from 1 across the coordinator, so
    /// "the oldest claimed spawn" is a comparison and not a clock.
    claim_seq: u64,
}

/// Every native record the coordinator holds.
#[derive(Debug, Default)]
pub(super) struct NativeRegistry {
    records: BTreeMap<String, NativeRecord>,
    claims: u64,
}

impl NativeRegistry {
    /// The dispatch reached a terminal state: its record goes with it.
    pub(super) fn forget(&mut self, dispatch_id: &str) {
        self.records.remove(dispatch_id);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RegisterOutcome {
    Registered,
    /// The prompt or message does not carry exactly this dispatch's marker.
    MarkerMissing,
    UnknownDispatch,
    WrongSession,
    Finished,
    /// The dispatch or its run was cancelled: relais no longer asks for it.
    Cancelled,
    AlreadyRegistered,
}

/// The hook's question on a marked call, answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ClaimOutcome {
    /// Run this spawn: these fields, whatever the call asked for.
    Spawn {
        subagent_type: String,
        model: String,
        prompt: String,
    },
    /// Send this message to this agent.
    Continue {
        agent_id: String,
        message: String,
    },
    UnknownDispatch,
    WrongSession,
    /// A second call for one request: only the first claims it.
    AlreadyClaimed,
    Finished,
    /// The dispatch or its run was cancelled, and the cancellation has not
    /// been acknowledged yet: the dispatch is still on record, but relais
    /// no longer stands behind it, so nothing may run under it.
    Cancelled,
    /// The dispatch's last heartbeat is older than the agent lease ttl:
    /// relais is no longer heartbeating, so nothing may run under it.
    LeaseLapsed,
    /// The call's recipient is not the one relais asked for (or a spawn
    /// was addressed to someone).
    WrongRecipient,
    /// The dispatch is bound to a process, which a lease cannot replace.
    NotLeasable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WorktreeOutcome {
    /// The tree relais prepared for the spawn it just claimed.
    Path { path: PathBuf },
    /// No claimed spawn is waiting for a tree in that session.
    NotNative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum BindNativeOutcome {
    Bound,
    /// The dispatch cannot run as asked; it is recorded `Failed`.
    Failed {
        reason: String,
    },
    /// No record in a state that expects this bind.
    NotNative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum StopNativeOutcome {
    Stopped { dispatch_id: String },
    NotNative,
}

/// What `native_status` knows about a dispatch: what N2 polls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "progress", rename_all = "snake_case")]
pub enum NativeProgress {
    Known {
        state: NativeState,
    },
    /// The dispatch reached a terminal state and its record went.
    Finished,
    Unknown,
}

/// One answer per native request, so the wire needs one `Response` variant
/// for all of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum NativeAnswer {
    Registered { outcome: RegisterOutcome },
    Claimed { outcome: ClaimOutcome },
    Worktree { outcome: WorktreeOutcome },
    Bound { outcome: BindNativeOutcome },
    Stopped { outcome: StopNativeOutcome },
    Agent { native: bool },
    Progress { progress: NativeProgress },
}

/// One reader per answer, so a caller names the answer it expects and
/// gets `None` for any other, without a catch-all over this enum.
impl NativeAnswer {
    pub fn registered(self) -> Option<RegisterOutcome> {
        if let Self::Registered { outcome } = self {
            Some(outcome)
        } else {
            None
        }
    }

    pub fn claimed(self) -> Option<ClaimOutcome> {
        if let Self::Claimed { outcome } = self {
            Some(outcome)
        } else {
            None
        }
    }

    pub fn worktree(self) -> Option<WorktreeOutcome> {
        if let Self::Worktree { outcome } = self {
            Some(outcome)
        } else {
            None
        }
    }

    pub fn bound(self) -> Option<BindNativeOutcome> {
        if let Self::Bound { outcome } = self {
            Some(outcome)
        } else {
            None
        }
    }

    pub fn stopped(self) -> Option<StopNativeOutcome> {
        if let Self::Stopped { outcome } = self {
            Some(outcome)
        } else {
            None
        }
    }

    pub fn agent(self) -> Option<bool> {
        if let Self::Agent { native } = self {
            Some(native)
        } else {
            None
        }
    }

    pub fn progress(self) -> Option<NativeProgress> {
        if let Self::Progress { progress } = self {
            Some(progress)
        } else {
            None
        }
    }
}

impl AdmissionState {
    /// Record what relais asked the session to run for a dispatch the
    /// runner already admitted.
    pub fn register_native(
        &mut self,
        session_id: &str,
        dispatch_id: &str,
        ask: &NativeAsk,
    ) -> RegisterOutcome {
        if find_marker(ask.marked_text()) != Marker::One(dispatch_id.to_string()) {
            return RegisterOutcome::MarkerMissing;
        }
        let Some(dispatch) = self.dispatches.get(dispatch_id) else {
            return if self.finished.contains(dispatch_id) {
                RegisterOutcome::Finished
            } else {
                RegisterOutcome::UnknownDispatch
            };
        };
        if dispatch.session_id != session_id {
            return RegisterOutcome::WrongSession;
        }
        if self.cancelled_dispatch(dispatch_id) {
            return RegisterOutcome::Cancelled;
        }
        if self.native.records.contains_key(dispatch_id) {
            return RegisterOutcome::AlreadyRegistered;
        }
        let worktree = match ask {
            NativeAsk::Spawn { worktree, .. } => Some(worktree.clone()),
            NativeAsk::Continue { .. } => None,
        };
        self.native.records.insert(
            dispatch_id.to_string(),
            NativeRecord {
                session_id: session_id.to_string(),
                state: NativeState::Requested { ask: ask.clone() },
                worktree,
                claim_seq: 0,
            },
        );
        RegisterOutcome::Registered
    }

    /// A marked call asks what to run. `to` is the recipient of a
    /// `SendMessage`, `None` for a spawn.
    pub fn claim_native(
        &mut self,
        session_id: &str,
        dispatch_id: &str,
        to: Option<&str>,
        now: Instant,
    ) -> ClaimOutcome {
        let Some(dispatch) = self.dispatches.get(dispatch_id) else {
            return if self.finished.contains(dispatch_id) {
                ClaimOutcome::Finished
            } else {
                ClaimOutcome::UnknownDispatch
            };
        };
        if dispatch.session_id != session_id {
            return ClaimOutcome::WrongSession;
        }
        if self.cancelled_dispatch(dispatch_id) {
            return ClaimOutcome::Cancelled;
        }
        let heartbeat_age = now.saturating_duration_since(dispatch.last_heartbeat);
        let Some(record) = self.native.records.get(dispatch_id) else {
            return ClaimOutcome::UnknownDispatch;
        };
        let NativeState::Requested { ask } = record.state.clone() else {
            return ClaimOutcome::AlreadyClaimed;
        };
        if heartbeat_age >= self.agent_lease_ttl {
            return ClaimOutcome::LeaseLapsed;
        }
        match (ask, to) {
            (
                NativeAsk::Spawn {
                    subagent_type,
                    model,
                    prompt,
                    ..
                },
                None,
            ) => {
                self.native.claims += 1;
                let seq = self.native.claims;
                self.set_native(dispatch_id, NativeState::Claimed, Some(seq));
                ClaimOutcome::Spawn {
                    subagent_type,
                    model,
                    prompt,
                }
            }
            (NativeAsk::Continue { agent_id, message }, Some(recipient))
                if recipient == agent_id =>
            {
                match self.lease_binding(dispatch_id, &agent_id, now) {
                    BindOutcome::Bound => {
                        self.native.claims += 1;
                        let seq = self.native.claims;
                        let bound = NativeState::Bound {
                            agent_id: agent_id.clone(),
                        };
                        self.set_native(dispatch_id, bound, Some(seq));
                        ClaimOutcome::Continue { agent_id, message }
                    }
                    BindOutcome::UnknownDispatch
                    | BindOutcome::PidNotAlive
                    | BindOutcome::AlreadyBoundToProcess => ClaimOutcome::NotLeasable,
                }
            }
            (NativeAsk::Spawn { .. }, Some(_)) | (NativeAsk::Continue { .. }, None | Some(_)) => {
                ClaimOutcome::WrongRecipient
            }
        }
    }

    /// The dispatch, or the run it belongs to, was cancelled. Ordinary
    /// admission refuses such a run (`RunCancelled`); a native call is
    /// refused the same way, before the cancellation is acknowledged and
    /// the dispatch leaves the map.
    fn cancelled_dispatch(&self, dispatch_id: &str) -> bool {
        self.dispatches.get(dispatch_id).is_some_and(|dispatch| {
            dispatch.cancellation.cancelled()
                || self
                    .runs
                    .get(&dispatch.run_id)
                    .is_some_and(|run| run.cancelled)
        })
    }

    /// `WorktreeCreate`: the oldest claimed spawn of the session that has
    /// no tree yet gets the name, and its prepared tree is the answer.
    pub fn native_worktree(&mut self, session_id: &str, name: &str) -> WorktreeOutcome {
        let oldest = self
            .native
            .records
            .iter()
            .filter(|(_, record)| {
                record.session_id == session_id && record.state == NativeState::Claimed
            })
            .min_by_key(|(_, record)| record.claim_seq)
            .map(|(dispatch_id, record)| (dispatch_id.clone(), record.worktree.clone()));
        let Some((dispatch_id, Some(path))) = oldest else {
            return WorktreeOutcome::NotNative;
        };
        let given = NativeState::TreeGiven {
            name: name.to_string(),
        };
        self.set_native(&dispatch_id, given, None);
        WorktreeOutcome::Path { path }
    }

    /// A marked Agent `PostToolUse` names the agent it launched.
    pub fn bind_native(
        &mut self,
        session_id: &str,
        dispatch_id: &str,
        agent_id: &str,
        now: Instant,
    ) -> BindNativeOutcome {
        let Some(record) = self.native.records.get(dispatch_id) else {
            return BindNativeOutcome::NotNative;
        };
        if record.session_id != session_id {
            return BindNativeOutcome::NotNative;
        }
        let name = match &record.state {
            NativeState::TreeGiven { name } => name.clone(),
            // The agent's `SubagentStop` was handled before this bind (two
            // hook processes race) and already stopped the record by its
            // tree's name. The lease is still bound, so the seat is
            // accounted for until the runner settles; the record keeps
            // the stop.
            NativeState::Stopped {
                agent_id: stopped, ..
            } if stopped == agent_id => {
                return match self.lease_binding(dispatch_id, agent_id, now) {
                    BindOutcome::Bound => BindNativeOutcome::Bound,
                    BindOutcome::UnknownDispatch
                    | BindOutcome::PidNotAlive
                    | BindOutcome::AlreadyBoundToProcess => {
                        self.fail_native(dispatch_id, BIND_REFUSED)
                    }
                };
            }
            NativeState::Requested { .. }
            | NativeState::Claimed
            | NativeState::Bound { .. }
            | NativeState::Stopped { .. }
            | NativeState::Failed { .. } => return BindNativeOutcome::NotNative,
        };
        // Both trees are relais's, and nothing is cleaned up here: the
        // dispatch is failed and the runner decides what comes next.
        if name != format!("agent-{agent_id}") {
            return self.fail_native(dispatch_id, TREE_MISMATCH);
        }
        match self.lease_binding(dispatch_id, agent_id, now) {
            BindOutcome::Bound => {
                let bound = NativeState::Bound {
                    agent_id: agent_id.to_string(),
                };
                self.set_native(dispatch_id, bound, None);
                BindNativeOutcome::Bound
            }
            BindOutcome::UnknownDispatch
            | BindOutcome::PidNotAlive
            | BindOutcome::AlreadyBoundToProcess => self.fail_native(dispatch_id, BIND_REFUSED),
        }
    }

    /// `SubagentStop`: the dispatch of that session currently bound to
    /// that agent stopped. It is not settled or released here.
    pub fn stop_native(
        &mut self,
        session_id: &str,
        agent_id: &str,
        transcript_path: Option<PathBuf>,
        last_assistant_message: Option<String>,
    ) -> StopNativeOutcome {
        // Bound to that agent, or not yet bound but given the tree named
        // after it: the stop raced ahead of the binding `PostToolUse`
        // (separate hook processes), and `name` is `agent-<agent_id>`.
        let tree_name = format!("agent-{agent_id}");
        let bound_to_agent = |record: &NativeRecord| {
            record.session_id == session_id
                && match &record.state {
                    NativeState::Bound { agent_id: bound } => bound == agent_id,
                    NativeState::TreeGiven { name } => *name == tree_name,
                    NativeState::Requested { .. }
                    | NativeState::Claimed
                    | NativeState::Stopped { .. }
                    | NativeState::Failed { .. } => false,
                }
        };
        let found = self
            .native
            .records
            .iter()
            .find(|(_, record)| bound_to_agent(record))
            .map(|(dispatch_id, _)| dispatch_id.clone());
        let Some(dispatch_id) = found else {
            return StopNativeOutcome::NotNative;
        };
        let stopped = NativeState::Stopped {
            agent_id: agent_id.to_string(),
            transcript_path,
            last_assistant_message,
        };
        self.set_native(&dispatch_id, stopped, None);
        StopNativeOutcome::Stopped { dispatch_id }
    }

    /// Is the agent bound, or was it, to a native dispatch still on
    /// record? Then the session may not talk to it behind relais's back.
    pub fn is_native_agent(&self, session_id: &str, agent_id: &str) -> bool {
        self.native.records.values().any(|record| {
            record.session_id == session_id
                && match &record.state {
                    NativeState::Bound { agent_id: held }
                    | NativeState::Stopped { agent_id: held, .. } => held == agent_id,
                    NativeState::Requested { .. }
                    | NativeState::Claimed
                    | NativeState::TreeGiven { .. }
                    | NativeState::Failed { .. } => false,
                }
        })
    }

    /// What N2 polls.
    pub fn native_status(&self, dispatch_id: &str) -> NativeProgress {
        match self.native.records.get(dispatch_id) {
            Some(record) => NativeProgress::Known {
                state: record.state.clone(),
            },
            None if self.finished.contains(dispatch_id) => NativeProgress::Finished,
            None => NativeProgress::Unknown,
        }
    }

    fn set_native(&mut self, dispatch_id: &str, state: NativeState, claim_seq: Option<u64>) {
        if let Some(record) = self.native.records.get_mut(dispatch_id) {
            record.state = state;
            if let Some(seq) = claim_seq {
                record.claim_seq = seq;
            }
        }
    }

    fn fail_native(&mut self, dispatch_id: &str, reason: &str) -> BindNativeOutcome {
        let failed = NativeState::Failed {
            reason: reason.to_string(),
        };
        self.set_native(dispatch_id, failed, None);
        BindNativeOutcome::Failed {
            reason: reason.to_string(),
        }
    }

    /// Bind the dispatch's agent lease. It never applies the end of an
    /// agent remembered by `settle_by_agent`: a native dispatch settles
    /// when the runner says so, with the real spend.
    fn lease_binding(&mut self, dispatch_id: &str, agent_id: &str, now: Instant) -> BindOutcome {
        self.bind_lease_only(dispatch_id, agent_id, Provenance::Known, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{
        Decision, DispatchRequest, DispatchSource, ResourceClass, RunRegistration,
    };
    use crate::native::marker_line;
    use crate::policy::ConcurrencyLimits;
    use std::time::Duration;

    fn state() -> AdmissionState {
        AdmissionState::new(ConcurrencyLimits {
            max_active_agents: None,
            max_active_agents_per_session: None,
            max_heavy_commands: None,
            max_training_jobs: None,
            max_agent_depth: None,
            max_agents_per_run: None,
            training_when_idle: true,
        })
    }

    fn admitted(state: &mut AdmissionState, dispatch: &str, session: &str, now: Instant) {
        state.register_run(
            &RunRegistration {
                run_id: "run".into(),
                session_id: session.into(),
                budget_micros: None,
                max_agents: None,
                max_depth: None,
            },
            now,
        );
        let decision = state.request(
            &DispatchRequest {
                dispatch_id: dispatch.into(),
                run_id: "run".into(),
                session_id: session.into(),
                parent_dispatch: None,
                depth: 0,
                resource: ResourceClass::ModelWork,
                reserve_micros: 0,
                source: DispatchSource::ManagedRun,
                caller_agent_id: None,
            },
            now,
        );
        assert_eq!(decision, Decision::Granted);
    }

    fn spawn_ask(dispatch: &str) -> NativeAsk {
        NativeAsk::Spawn {
            subagent_type: "relais-worker".into(),
            model: "sonnet".into(),
            prompt: format!("do it\n{}", marker_line(dispatch)),
            worktree: PathBuf::from("/trees/task"),
        }
    }

    fn continue_ask(dispatch: &str, agent: &str) -> NativeAsk {
        NativeAsk::Continue {
            agent_id: agent.into(),
            message: format!("fix it\n{}", marker_line(dispatch)),
        }
    }

    #[test]
    fn registration_needs_an_admitted_unfinished_dispatch_of_the_session_with_its_marker() {
        let now = Instant::now();
        let mut state = state();
        admitted(&mut state, "d1", "s1", now);
        let ask = spawn_ask("d1");
        assert_eq!(
            state.register_native("s1", "nope", &spawn_ask("nope")),
            RegisterOutcome::UnknownDispatch
        );
        assert_eq!(
            state.register_native("s2", "d1", &ask),
            RegisterOutcome::WrongSession
        );
        assert_eq!(
            state.register_native("s1", "d1", &spawn_ask("d2")),
            RegisterOutcome::MarkerMissing
        );
        assert_eq!(
            state.register_native("s1", "d1", &ask),
            RegisterOutcome::Registered
        );
        assert_eq!(
            state.register_native("s1", "d1", &ask),
            RegisterOutcome::AlreadyRegistered
        );
        state.settle("d1", Some(0), now);
        state.release("d1", now);
        assert_eq!(
            state.register_native("s1", "d1", &ask),
            RegisterOutcome::Finished
        );
    }

    /// Between a cancellation and its acknowledgement the dispatch is
    /// still on record; nothing may be registered or run under it.
    #[test]
    fn a_cancelled_run_cannot_be_registered_or_claimed() {
        let now = Instant::now();
        let mut state = state();
        admitted(&mut state, "d1", "s1", now);
        admitted(&mut state, "d2", "s1", now);
        state.register_native("s1", "d1", &spawn_ask("d1"));
        state.cancel_run("run", now);
        assert_eq!(
            state.claim_native("s1", "d1", None, now),
            ClaimOutcome::Cancelled
        );
        assert_eq!(
            state.register_native("s1", "d2", &spawn_ask("d2")),
            RegisterOutcome::Cancelled
        );
    }

    #[test]
    fn a_cancelled_dispatch_cannot_be_claimed() {
        let now = Instant::now();
        let mut state = state();
        admitted(&mut state, "d1", "s1", now);
        state.register_native("s1", "d1", &spawn_ask("d1"));
        state.cancel_dispatch("d1", now);
        assert_eq!(
            state.claim_native("s1", "d1", None, now),
            ClaimOutcome::Cancelled
        );
    }

    /// `SubagentStop` and the binding `PostToolUse` are separate hook
    /// processes. A stop handled first still stops the record (by its
    /// tree's name), and the later bind keeps that stop.
    #[test]
    fn a_stop_that_races_ahead_of_the_bind_still_stops_the_record() {
        let now = Instant::now();
        let mut state = state();
        admitted(&mut state, "d1", "s1", now);
        state.register_native("s1", "d1", &spawn_ask("d1"));
        state.claim_native("s1", "d1", None, now);
        state.native_worktree("s1", "agent-a1");
        assert_eq!(
            state.stop_native("s1", "a1", None, Some("done".into())),
            StopNativeOutcome::Stopped {
                dispatch_id: "d1".into()
            }
        );
        assert_eq!(
            state.bind_native("s1", "d1", "a1", now),
            BindNativeOutcome::Bound
        );
        assert!(matches!(
            state.native_status("d1"),
            NativeProgress::Known {
                state: NativeState::Stopped { ref agent_id, .. }
            } if agent_id == "a1"
        ));
        // The stop never went through `settle_by_agent`, so nothing is
        // remembered to settle some later dispatch of this agent.
        assert!(state.stopped.is_empty());
    }

    #[test]
    fn a_spawn_is_claimed_once_given_its_tree_bound_and_stopped() {
        let now = Instant::now();
        let mut state = state();
        admitted(&mut state, "d1", "s1", now);
        state.register_native("s1", "d1", &spawn_ask("d1"));
        assert!(matches!(
            state.claim_native("s1", "d1", None, now),
            ClaimOutcome::Spawn { .. }
        ));
        assert_eq!(
            state.claim_native("s1", "d1", None, now),
            ClaimOutcome::AlreadyClaimed
        );
        assert_eq!(
            state.native_worktree("s1", "agent-a1"),
            WorktreeOutcome::Path {
                path: PathBuf::from("/trees/task")
            }
        );
        assert_eq!(
            state.native_worktree("s1", "agent-a2"),
            WorktreeOutcome::NotNative,
            "one tree per claimed spawn"
        );
        assert_eq!(
            state.bind_native("s1", "d1", "a1", now),
            BindNativeOutcome::Bound
        );
        assert!(state.is_native_agent("s1", "a1"));
        assert!(!state.is_native_agent("s2", "a1"));
        assert_eq!(
            state.stop_native("s1", "a1", Some("/t.jsonl".into()), Some("done".into())),
            StopNativeOutcome::Stopped {
                dispatch_id: "d1".into()
            }
        );
        assert!(
            state.is_native_agent("s1", "a1"),
            "was bound, still on record"
        );
        assert_eq!(
            state.native_status("d1"),
            NativeProgress::Known {
                state: NativeState::Stopped {
                    agent_id: "a1".into(),
                    transcript_path: Some("/t.jsonl".into()),
                    last_assistant_message: Some("done".into()),
                }
            }
        );
        assert!(
            state.dispatches.contains_key("d1"),
            "a stop does not settle: the runner does, with the real spend"
        );
        state.settle("d1", Some(5), now);
        state.release("d1", now);
        assert_eq!(state.native_status("d1"), NativeProgress::Finished);
        assert!(!state.is_native_agent("s1", "a1"));
    }

    #[test]
    fn the_oldest_claimed_spawn_gets_the_first_tree() {
        let now = Instant::now();
        let mut state = state();
        admitted(&mut state, "d1", "s1", now);
        admitted(&mut state, "d2", "s1", now);
        let second_tree = NativeAsk::Spawn {
            subagent_type: "w".into(),
            model: "m".into(),
            prompt: marker_line("d2"),
            worktree: PathBuf::from("/trees/two"),
        };
        state.register_native("s1", "d1", &spawn_ask("d1"));
        state.register_native("s1", "d2", &second_tree);
        // d1 was registered first, d2 claimed first.
        state.claim_native("s1", "d2", None, now);
        state.claim_native("s1", "d1", None, now);
        assert_eq!(
            state.native_worktree("s1", "agent-x"),
            WorktreeOutcome::Path {
                path: PathBuf::from("/trees/two")
            }
        );
        assert_eq!(
            state.native_worktree("s1", "agent-y"),
            WorktreeOutcome::Path {
                path: PathBuf::from("/trees/task")
            }
        );
    }

    #[test]
    fn a_tree_named_for_another_agent_fails_the_dispatch() {
        let now = Instant::now();
        let mut state = state();
        admitted(&mut state, "d1", "s1", now);
        state.register_native("s1", "d1", &spawn_ask("d1"));
        state.claim_native("s1", "d1", None, now);
        state.native_worktree("s1", "agent-a1");
        assert_eq!(
            state.bind_native("s1", "d1", "a2", now),
            BindNativeOutcome::Failed {
                reason: "native_tree_mismatch".into()
            }
        );
        assert_eq!(
            state.native_status("d1"),
            NativeProgress::Known {
                state: NativeState::Failed {
                    reason: "native_tree_mismatch".into()
                }
            }
        );
    }

    #[test]
    fn a_continuation_binds_at_once_and_only_for_its_agent() {
        let now = Instant::now();
        let mut state = state();
        admitted(&mut state, "d2", "s1", now);
        state.register_native("s1", "d2", &continue_ask("d2", "a1"));
        assert_eq!(
            state.claim_native("s1", "d2", Some("a9"), now),
            ClaimOutcome::WrongRecipient
        );
        assert_eq!(
            state.claim_native("s1", "d2", None, now),
            ClaimOutcome::WrongRecipient
        );
        assert!(matches!(
            state.claim_native("s1", "d2", Some("a1"), now),
            ClaimOutcome::Continue { .. }
        ));
        assert_eq!(
            state.native_status("d2"),
            NativeProgress::Known {
                state: NativeState::Bound {
                    agent_id: "a1".into()
                }
            }
        );
    }

    #[test]
    fn a_dispatch_whose_heartbeats_stopped_cannot_be_claimed() {
        let t0 = Instant::now();
        let mut state = state();
        admitted(&mut state, "d1", "s1", t0);
        state.register_native("s1", "d1", &spawn_ask("d1"));
        let ttl = state.agent_lease_ttl();
        let late = t0 + ttl + Duration::from_secs(1);
        assert_eq!(
            state.claim_native("s1", "d1", None, late),
            ClaimOutcome::LeaseLapsed
        );
        state.heartbeat("d1", late);
        assert!(matches!(
            state.claim_native("s1", "d1", None, late + Duration::from_secs(1)),
            ClaimOutcome::Spawn { .. }
        ));
    }

    #[test]
    fn a_stopped_continue_is_not_confused_with_the_first_attempt() {
        let now = Instant::now();
        let mut state = state();
        admitted(&mut state, "d1", "s1", now);
        state.register_native("s1", "d1", &spawn_ask("d1"));
        state.claim_native("s1", "d1", None, now);
        state.native_worktree("s1", "agent-a1");
        state.bind_native("s1", "d1", "a1", now);
        state.stop_native("s1", "a1", None, Some("first".into()));
        admitted(&mut state, "d2", "s1", now);
        state.register_native("s1", "d2", &continue_ask("d2", "a1"));
        state.claim_native("s1", "d2", Some("a1"), now);
        assert_eq!(
            state.stop_native("s1", "a1", None, Some("second".into())),
            StopNativeOutcome::Stopped {
                dispatch_id: "d2".into()
            }
        );
        assert_eq!(
            state.stop_native("s1", "a1", None, None),
            StopNativeOutcome::NotNative,
            "both attempts are stopped now"
        );
    }
}
