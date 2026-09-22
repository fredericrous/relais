//! The attempt lifecycle as a pure function (SPEC §9).
//!
//! The spec's observation table — "required checks and review pass on
//! the same candidate → accepted", "same failure and unchanged candidate
//! recur → escalate or fail immediately", and the rest — is `decide`: a
//! total function from (what the run may still spend, what was observed)
//! to (the next state, why, and what to do next). It touches no git, no
//! SQLite, no process. The runner observes, calls `decide`, records the
//! transition it is handed, and performs the one effect the step names.
//!
//! Every row of the table is therefore a unit test over values, and the
//! runner's own tests are free to be about IO.

use serde::{Deserialize, Serialize};

use crate::lifecycle::{Reason, State};
use crate::policy::{BlockCode, Tier};

/// What kind of attempt is being dispatched (SPEC §9: initial, one
/// repair, one stronger attempt).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptKind {
    Initial,
    Repair,
    Escalation,
}

impl AttemptKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Repair => "repair",
            Self::Escalation => "escalation",
        }
    }
}

/// How a run ended. The terminal half of `RunOutcome`; the receipt
/// travels with acceptance and nothing else.
#[derive(Debug, Clone, PartialEq)]
pub enum Terminal {
    Accepted(Box<crate::verify::Receipt>),
    NeedsDecision { reason: Reason, detail: String },
    NeedsReview { detail: String },
    Blocked { code: BlockCode, detail: String },
    Failed { detail: String },
    BudgetExhausted { detail: String },
    Interrupted { detail: String },
    Cancelled { detail: String },
}

impl Terminal {
    pub fn state(&self) -> State {
        match self {
            Self::Accepted(_) => State::Accepted,
            Self::NeedsDecision { .. } => State::NeedsDecision,
            Self::NeedsReview { .. } => State::NeedsReview,
            Self::Blocked { .. } => State::Blocked,
            Self::Failed { .. } => State::Failed,
            Self::BudgetExhausted { .. } => State::BudgetExhausted,
            Self::Interrupted { .. } => State::Interrupted,
            Self::Cancelled { .. } => State::Cancelled,
        }
    }

    /// What becomes of the run's worktree at this end (SPEC §8, §12).
    pub fn worktree_end(&self) -> WorktreeEnd {
        match self {
            // Uncertain state: a writer may still be in the tree, and
            // nothing was snapshotted. `relais resume --retire` retires
            // it once the dispatches are provably dead.
            Self::Interrupted { .. } => WorktreeEnd::Keep,
            Self::Accepted(_)
            | Self::NeedsDecision { .. }
            | Self::NeedsReview { .. }
            | Self::Blocked { .. }
            | Self::Failed { .. }
            | Self::BudgetExhausted { .. }
            | Self::Cancelled { .. } => WorktreeEnd::Retire,
        }
    }

    /// The human line for a non-accepted end.
    pub fn detail(&self) -> &str {
        match self {
            Self::Accepted(_) => "",
            Self::NeedsDecision { detail, .. }
            | Self::NeedsReview { detail }
            | Self::Blocked { detail, .. }
            | Self::Failed { detail }
            | Self::BudgetExhausted { detail }
            | Self::Interrupted { detail }
            | Self::Cancelled { detail } => detail,
        }
    }
}

/// What becomes of a run's worktree at its end: retired — everything
/// tracked named and exported, then the directory removed — or kept,
/// for the one end whose tree may still be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeEnd {
    Retire,
    Keep,
}

/// What the run may still do, as the runner knows it before deciding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub attempts_used: u32,
    pub max_attempts: u32,
    pub repairs_used: u32,
    pub max_repairs: u32,
    pub tier: Tier,
    /// The stronger tier escalation may buy, when authorized.
    pub escalation_tier: Option<Tier>,
}

impl Budget {
    fn escalation_authorized(&self) -> bool {
        self.escalation_tier
            .is_some_and(|stronger| stronger > self.tier)
    }
}

/// Which ceiling a run hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Limit {
    Attempts {
        max: u32,
        last_failures: Vec<String>,
    },
    WallClock,
    Spend {
        spent: String,
        ceiling: String,
    },
    /// The coordinator refused more work: budget, depth or the agent cap.
    Admission {
        code: String,
        detail: String,
    },
    /// Queued for admission until the wall clock ran out.
    AdmissionQueue {
        position: usize,
    },
}

/// One row's worth of input: what the runner observed about the
/// candidate or the dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// Required checks passed and, when review was required, it passed.
    ChecksAndReviewPassed,
    /// Required checks failed on this candidate.
    VerificationFailed {
        failures: Vec<String>,
        /// The candidate is byte-identical to the previous attempt's.
        unchanged_candidate: bool,
        /// …and it failed the same checks.
        same_failures: bool,
        /// Every failure also fails at the base revision.
        all_preexisting: bool,
    },
    /// A required check is a gap: skipped, inert, unavailable, untrusted.
    VerificationGap(Vec<String>),
    /// The diff leaves the contract's write scope.
    ScopeViolation(Vec<String>),
    /// The worker proposed that something outside the task blocks it.
    WorkerBlockage(String),
    /// The reviewer reported findings.
    ReviewFindings(String),
    /// Required review could not be obtained.
    ReviewUnavailable(String),
    /// The worker process ended without a terminal result: killed,
    /// non-zero exit, or output the adapter could not read.
    TerminalResultMissing { timed_out: bool, detail: String },
    /// The harness refused the worker these tools. A worker that could
    /// not act is blocked, not failed, and never escalated (SPEC §8).
    PermissionDenied(Vec<String>),
    /// The dispatch was cancelled through the coordinator.
    Cancelled(String),
    /// The provider ran a model other than the one requested.
    UnapprovedSubstitution {
        requested: String,
        effective: String,
    },
    /// A ceiling was reached before or during dispatch.
    LimitReached(Limit),
}

/// What to do after recording the transition.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    /// Dispatch another attempt of this kind at this tier.
    Attempt {
        kind: AttemptKind,
        tier: Tier,
    },
    /// Build the receipt for the verified candidate.
    Accept,
    Stop(Terminal),
}

/// A row of the table, evaluated.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub state: State,
    pub reason: Reason,
    pub detail: serde_json::Value,
    pub next: Next,
}

impl Decision {
    fn stop(state: State, reason: Reason, detail: serde_json::Value, terminal: Terminal) -> Self {
        Self {
            state,
            reason,
            detail,
            next: Next::Stop(terminal),
        }
    }
}

/// The observation table (SPEC §9), as a function.
pub fn decide(budget: &Budget, observation: Observation) -> Decision {
    match observation {
        // The receipt is the runner's to build; the machine only says
        // the candidate is accepted.
        Observation::ChecksAndReviewPassed => Decision {
            state: State::Accepted,
            reason: Reason::ChecksAndReviewPassed,
            detail: serde_json::json!({ "attempts": budget.attempts_used }),
            next: Next::Accept,
        },

        Observation::VerificationFailed {
            failures,
            unchanged_candidate,
            same_failures,
            all_preexisting,
        } => {
            // Same failure on an unchanged candidate: another attempt of
            // the same kind would be a no-op. Fail immediately; buying a
            // stronger model for a worker that changed nothing is not
            // a repair (SPEC §9).
            if unchanged_candidate && same_failures {
                let detail = format!(
                    "same failure on an unchanged candidate: {}; failing immediately",
                    failures.join(", ")
                );
                return Decision::stop(
                    State::Failed,
                    Reason::SameFailureRecurrence,
                    serde_json::json!({ "failures": failures }),
                    Terminal::Failed { detail },
                );
            }
            // A behavioural failure with allowance left: one localized
            // repair. Pre-existing failures are chased too — a task may
            // exist precisely to fix them, and acceptance still requires
            // green checks or an explicit waiver.
            if budget.repairs_used < budget.max_repairs {
                return Decision {
                    state: State::Repairing,
                    reason: Reason::BehavioralFailure,
                    detail: serde_json::json!({
                        "failures": failures,
                        "attempt": budget.attempts_used,
                    }),
                    next: Next::Attempt {
                        kind: AttemptKind::Repair,
                        tier: budget.tier,
                    },
                };
            }
            // Repair exhausted: a stronger profile when authorized.
            if let Some(stronger) = budget
                .escalation_tier
                .filter(|_| budget.escalation_authorized())
            {
                return Decision {
                    state: State::Escalating,
                    reason: Reason::RepairExhausted,
                    detail: serde_json::json!({
                        "failures": failures,
                        "to_tier": stronger.as_str(),
                    }),
                    next: Next::Attempt {
                        kind: AttemptKind::Escalation,
                        tier: stronger,
                    },
                };
            }
            // Terminal. Existing failures are not automatically waived —
            // the waiver must already be in policy or become an explicit
            // contract revision (SPEC §10).
            if all_preexisting {
                let detail = format!(
                    "these checks also fail at the base and are not waived: {}",
                    failures.join(", ")
                );
                return Decision::stop(
                    State::NeedsDecision,
                    Reason::BaselineFailureNotWaived,
                    serde_json::json!({ "failures": failures }),
                    Terminal::NeedsDecision {
                        reason: Reason::BaselineFailureNotWaived,
                        detail,
                    },
                );
            }
            let detail = format!(
                "verification failed and no repair or escalation remains: {}",
                failures.join(", ")
            );
            Decision::stop(
                State::Failed,
                Reason::RepairExhausted,
                serde_json::json!({ "failures": failures }),
                Terminal::Failed { detail },
            )
        }

        Observation::VerificationGap(gaps) => {
            let detail = format!("required checks are gaps, not passes: {}", gaps.join("; "));
            Decision::stop(
                State::NeedsDecision,
                Reason::VerificationGap,
                serde_json::json!({ "gaps": gaps }),
                Terminal::NeedsDecision {
                    reason: Reason::VerificationGap,
                    detail,
                },
            )
        }

        Observation::ScopeViolation(paths) => {
            let detail = format!("the diff leaves the contract scope: {}", paths.join(", "));
            Decision::stop(
                State::NeedsDecision,
                Reason::ScopeExceeded,
                serde_json::json!({ "paths": paths }),
                Terminal::NeedsDecision {
                    reason: Reason::ScopeExceeded,
                    detail,
                },
            )
        }

        // The environment is never escalated to a stronger model.
        Observation::WorkerBlockage(claim) => {
            let detail = format!(
                "worker blockage: {}",
                claim.lines().last().unwrap_or(&claim)
            );
            Decision::stop(
                State::Blocked,
                Reason::EnvMissing,
                serde_json::json!({ "claim": claim }),
                Terminal::Blocked {
                    code: BlockCode::EnvMissing,
                    detail,
                },
            )
        }

        Observation::ReviewFindings(detail) => Decision::stop(
            State::NeedsReview,
            Reason::ReviewFindings,
            serde_json::json!({ "detail": detail }),
            Terminal::NeedsReview { detail },
        ),

        Observation::ReviewUnavailable(detail) => Decision::stop(
            State::NeedsReview,
            Reason::ReviewUnavailable,
            serde_json::json!({ "detail": detail }),
            Terminal::NeedsReview { detail },
        ),

        Observation::TerminalResultMissing { timed_out, detail } => Decision::stop(
            State::Interrupted,
            Reason::ProcessCrash,
            serde_json::json!({ "timed_out": timed_out, "detail": detail }),
            Terminal::Interrupted {
                detail: if detail.is_empty() {
                    "the worker process ended without a terminal result; state is \
                     interrupted and the worktree is preserved"
                        .into()
                } else {
                    format!(
                        "the worker process ended without a terminal result ({detail}); \
                         state is interrupted and the worktree is preserved"
                    )
                },
            },
        ),

        Observation::PermissionDenied(tools) => {
            let detail = format!(
                "the harness refused the worker these tools: {}; grant them in the machine \
                 permissions allowlist or narrow the task — a stronger model is not bought \
                 for a missing permission",
                tools.join(", ")
            );
            Decision::stop(
                State::Blocked,
                Reason::PermissionDenied,
                serde_json::json!({ "tools": tools }),
                Terminal::Blocked {
                    code: BlockCode::PermissionDenied,
                    detail,
                },
            )
        }

        Observation::Cancelled(detail) => Decision::stop(
            State::Cancelled,
            Reason::CancelledByUser,
            serde_json::json!({ "detail": detail }),
            Terminal::Cancelled { detail },
        ),

        Observation::UnapprovedSubstitution {
            requested,
            effective,
        } => {
            let detail = format!(
                "provider substituted {effective} for the requested {requested}; no further dispatch"
            );
            Decision::stop(
                State::Failed,
                Reason::UnapprovedSubstitution,
                serde_json::json!({ "requested": requested, "effective": effective }),
                Terminal::Failed { detail },
            )
        }

        Observation::LimitReached(limit) => {
            let (detail, evidence) = match &limit {
                Limit::Attempts { max, last_failures } => (
                    format!("attempt ceiling reached ({max}) with failures: {last_failures:?}"),
                    serde_json::json!({ "limit": "attempts", "max": max }),
                ),
                Limit::WallClock => (
                    "wall clock for the run is exhausted".to_string(),
                    serde_json::json!({ "limit": "wall_clock" }),
                ),
                // Both rendered amounts name themselves ("$0.02 per run",
                // "$0.03 today"): there is more than one spending
                // ceiling, and a receipt must say which one stopped the
                // run.
                Limit::Spend { spent, ceiling } => (
                    format!("spend ceiling reached ({spent} of {ceiling})"),
                    serde_json::json!({ "limit": "spend", "spent": spent, "ceiling": ceiling }),
                ),
                Limit::Admission { code, detail } => (
                    format!("{code}: {detail}"),
                    serde_json::json!({ "limit": "admission", "code": code }),
                ),
                Limit::AdmissionQueue { position } => (
                    format!(
                        "the wall clock ran out while queued for admission (position {position})"
                    ),
                    serde_json::json!({ "limit": "admission_queue", "position": position }),
                ),
            };
            Decision::stop(
                State::BudgetExhausted,
                Reason::LimitReached,
                evidence,
                Terminal::BudgetExhausted { detail },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(repairs_used: u32, escalation: Option<Tier>) -> Budget {
        Budget {
            attempts_used: 1,
            max_attempts: 3,
            repairs_used,
            max_repairs: 1,
            tier: Tier::Implementation,
            escalation_tier: escalation,
        }
    }

    fn failed(unchanged: bool, same: bool, preexisting: bool) -> Observation {
        Observation::VerificationFailed {
            failures: vec!["make@1".into()],
            unchanged_candidate: unchanged,
            same_failures: same,
            all_preexisting: preexisting,
        }
    }

    // SPEC §9 rows, one assertion each.

    #[test]
    fn a_behavioural_failure_with_allowance_repairs_at_the_same_tier() {
        let d = decide(
            &budget(0, Some(Tier::Escalation)),
            failed(false, false, false),
        );
        assert_eq!(d.state, State::Repairing);
        assert_eq!(d.reason, Reason::BehavioralFailure);
        assert_eq!(
            d.next,
            Next::Attempt {
                kind: AttemptKind::Repair,
                tier: Tier::Implementation
            }
        );
    }

    #[test]
    fn repair_exhausted_escalates_when_a_stronger_tier_is_authorized() {
        let d = decide(
            &budget(1, Some(Tier::Escalation)),
            failed(false, false, false),
        );
        assert_eq!(d.state, State::Escalating);
        assert_eq!(d.reason, Reason::RepairExhausted);
        assert_eq!(
            d.next,
            Next::Attempt {
                kind: AttemptKind::Escalation,
                tier: Tier::Escalation
            }
        );
    }

    #[test]
    fn repair_exhausted_without_a_stronger_tier_fails() {
        let d = decide(&budget(1, None), failed(false, false, false));
        assert_eq!(d.state, State::Failed);
        assert!(matches!(d.next, Next::Stop(Terminal::Failed { .. })));
        // Starting on the strongest profile does not create another tier.
        let top = Budget {
            tier: Tier::Escalation,
            escalation_tier: Some(Tier::Escalation),
            ..budget(1, None)
        };
        assert_eq!(
            decide(&top, failed(false, false, false)).state,
            State::Failed
        );
    }

    #[test]
    fn refused_tools_are_blocked_never_escalated() {
        let b = budget(0, Some(Tier::Escalation));
        let d = decide(&b, Observation::PermissionDenied(vec!["Edit".into()]));
        assert_eq!(d.state, State::Blocked);
        assert_eq!(d.reason, Reason::PermissionDenied);
        assert!(matches!(
            d.next,
            Next::Stop(Terminal::Blocked {
                code: BlockCode::PermissionDenied,
                ..
            })
        ));
    }

    #[test]
    fn same_failure_on_an_unchanged_candidate_fails_immediately() {
        // Even with a repair and an escalation still available.
        let d = decide(
            &budget(0, Some(Tier::Escalation)),
            failed(true, true, false),
        );
        assert_eq!(d.state, State::Failed);
        assert_eq!(d.reason, Reason::SameFailureRecurrence);
        // A changed candidate with the same failure is an ordinary failure.
        let d = decide(
            &budget(0, Some(Tier::Escalation)),
            failed(false, true, false),
        );
        assert_eq!(d.state, State::Repairing);
    }

    #[test]
    fn preexisting_failures_are_a_decision_not_a_waiver_once_nothing_remains() {
        let d = decide(&budget(1, None), failed(false, false, true));
        assert_eq!(d.state, State::NeedsDecision);
        assert_eq!(d.reason, Reason::BaselineFailureNotWaived);
        // …but with allowance left they are still chased first.
        assert_eq!(
            decide(&budget(0, None), failed(false, false, true)).state,
            State::Repairing
        );
    }

    #[test]
    fn the_environment_is_blocked_never_escalated() {
        let d = decide(
            &budget(0, Some(Tier::Escalation)),
            Observation::WorkerBlockage("relais-blocked: no network".into()),
        );
        assert_eq!(d.state, State::Blocked);
        assert!(matches!(
            d.next,
            Next::Stop(Terminal::Blocked {
                code: BlockCode::EnvMissing,
                ..
            })
        ));
    }

    #[test]
    fn scope_gap_review_crash_cancel_and_limits_each_land_on_their_row() {
        let b = budget(0, Some(Tier::Escalation));
        assert_eq!(
            decide(&b, Observation::ScopeViolation(vec!["x".into()])).state,
            State::NeedsDecision
        );
        assert_eq!(
            decide(&b, Observation::VerificationGap(vec!["g".into()])).reason,
            Reason::VerificationGap
        );
        assert_eq!(
            decide(&b, Observation::ReviewFindings("f".into())).state,
            State::NeedsReview
        );
        assert_eq!(
            decide(&b, Observation::ReviewUnavailable("u".into())).reason,
            Reason::ReviewUnavailable
        );
        assert_eq!(
            decide(
                &b,
                Observation::TerminalResultMissing {
                    timed_out: true,
                    detail: String::new()
                }
            )
            .state,
            State::Interrupted
        );
        assert_eq!(
            decide(&b, Observation::Cancelled("c".into())).state,
            State::Cancelled
        );
        assert_eq!(
            decide(
                &b,
                Observation::UnapprovedSubstitution {
                    requested: "sonnet".into(),
                    effective: "haiku".into()
                }
            )
            .reason,
            Reason::UnapprovedSubstitution
        );
        for limit in [
            Limit::Attempts {
                max: 3,
                last_failures: vec![],
            },
            Limit::WallClock,
            Limit::Spend {
                spent: "$1".into(),
                ceiling: "$1".into(),
            },
            Limit::Admission {
                code: "run_agent_cap".into(),
                detail: "".into(),
            },
            Limit::AdmissionQueue { position: 2 },
        ] {
            let d = decide(&b, Observation::LimitReached(limit));
            assert_eq!(d.state, State::BudgetExhausted);
            assert_eq!(d.reason, Reason::LimitReached);
        }
    }

    /// SPEC §8, §12: every end retires the worktree — its tree is a
    /// named candidate — except the one whose tree may still be being
    /// written.
    #[test]
    fn only_an_interrupted_end_keeps_its_worktree() {
        let detail = || "why".to_string();
        assert_eq!(
            Terminal::Interrupted { detail: detail() }.worktree_end(),
            WorktreeEnd::Keep
        );
        for retired in [
            Terminal::NeedsDecision {
                reason: Reason::ScopeExceeded,
                detail: detail(),
            },
            Terminal::NeedsReview { detail: detail() },
            Terminal::Blocked {
                code: BlockCode::DirtyBase,
                detail: detail(),
            },
            Terminal::Failed { detail: detail() },
            Terminal::BudgetExhausted { detail: detail() },
            Terminal::Cancelled { detail: detail() },
        ] {
            assert_eq!(retired.worktree_end(), WorktreeEnd::Retire, "{retired:?}");
        }
    }
}
