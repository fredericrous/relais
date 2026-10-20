//! What a run has spent and its per-attempt progress.

use super::*;

/// What a run has spent so far, and how well that figure is known.
///
/// One value rather than two out-parameters: every place that folds a
/// dispatch's cost in also folds its completeness, and passing them
/// separately is how the decomposed path came to pass a fresh zero as
/// the spend while keeping the real completeness (R1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunSpend {
    pub(crate) total: MicroUsd,
    pub(crate) completeness: CostCompleteness,
}

impl RunSpend {
    /// Nothing spent, and that is an exact figure.
    pub(crate) fn zero() -> Self {
        Self {
            total: MicroUsd::ZERO,
            completeness: CostCompleteness::Actual,
        }
    }

    /// Fold one dispatch's usage in. Unknown cost is not zero: the total
    /// stays what was reported and the completeness says it is a lower
    /// bound (SPEC §11).
    pub(crate) fn fold(&mut self, cost: Option<MicroUsd>, completeness: CostCompleteness) {
        if let Some(cost) = cost {
            self.total += cost;
        }
        self.completeness = self.completeness.max(completeness);
    }
}

/// Which spending ceiling a check is about. `Limit::Spend` carries two
/// rendered amounts and nothing else, so the ceiling names itself —
/// otherwise a receipt cannot say which one stopped the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ceiling {
    PerRun,
    PerDay,
}

impl Ceiling {
    pub(super) fn render(self, spent: MicroUsd, ceiling: MicroUsd) -> (String, String) {
        match self {
            Self::PerRun => (spent.to_string(), format!("{ceiling} per run")),
            Self::PerDay => (format!("{spent} today"), format!("{ceiling} per day")),
        }
    }
}

pub(crate) fn spend_limit(spent: MicroUsd, ceiling: MicroUsd, which: Ceiling) -> Limit {
    let (spent, ceiling) = which.render(spent, ceiling);
    Limit::Spend { spent, ceiling }
}

pub(crate) fn effort_str(effort: Option<&EffortId>) -> Option<String> {
    effort.map(EffortId::as_str).map(str::to_string)
}

/// The start of the UTC day containing an RFC3339 instant, formatted the
/// way the ledger stamps its rows, so the comparison is a comparison of
/// like strings. `None` when the input is not RFC3339.
pub(crate) fn utc_day_start(now_rfc3339: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(now_rfc3339).ok()?;
    Some(
        parsed
            .with_timezone(&chrono::Utc)
            .date_naive()
            .and_hms_opt(0, 0, 0)?
            .and_utc()
            .to_rfc3339(),
    )
}

/// The per-attempt bookkeeping the loop threads through the machine.
pub(super) struct Progress {
    pub(super) budget: Budget,
    pub(super) kind: AttemptKind,
    pub(super) last_failures: Option<Vec<String>>,
    pub(super) last_candidate: Option<String>,
    pub(super) spend: RunSpend,
    pub(super) models_used: Vec<String>,
    /// The model the last attempt booked a native record of that has no
    /// price: its cost settled as the whole reservation, so no further
    /// attempt is admitted (SPEC §11).
    pub(super) unpriced_model: Option<Vec<String>>,
}

/// The state a run starts its attempts from.
pub(super) fn fresh_progress(preflight: &Preflight) -> Progress {
    let authority = &preflight.authority;
    let decision = &preflight.decision;
    Progress {
        budget: Budget {
            attempts_used: 0,
            max_attempts: authority.max_attempts,
            repairs_used: 0,
            max_repairs: authority.max_repairs_before_escalation,
            tier: decision.tier,
            rung: RungIndex::INITIAL,
            // The route's kept tier: `None` when `allowed_models`
            // removed its model. A budget that cannot reach the rung
            // still decides an escalation, and the attempt ceiling
            // ends the run before that rung is ever read.
            escalation_tier: decision.escalation_tier,
        },
        kind: AttemptKind::Initial,
        last_failures: None,
        last_candidate: None,
        spend: RunSpend::zero(),
        models_used: Vec::new(),
        unpriced_model: None,
    }
}
