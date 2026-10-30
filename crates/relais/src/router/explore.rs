//! Exploration's evidence (learning-loop Phase 4, SPEC §30): which tasks the
//! draw lowered ("drawn") and which it passed over ("undrawn"), counted per
//! stratum (kind x difficulty band x scope), the automatic stop rule, and the
//! projection `relais report` prints. Pure: the ledger adapter reads the
//! rows, this module counts them.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use super::outcome::TaskOutcome;
pub use crate::ledger::ExplorationTask;

/// Drawn tasks with a strong outcome the stop rule waits for, and the
/// count a stratum must reach before the learner may use it (Phase 8).
pub const MIN_DRAWN: usize = 20;

/// The stop fires when drawn tasks' correction-or-failure rate exceeds the
/// undrawn tasks' by more than this (a fraction: 15 points).
pub const STOP_MARGIN: f64 = 0.15;

/// The difficulty band a stratum is keyed by.
pub fn band(difficulty: u8) -> &'static str {
    match difficulty {
        0..=2 => "d1-2",
        3 => "d3",
        _ => "d4-5",
    }
}

/// Whether an outcome counts as strong evidence (every one but `unknown`
/// and `answered`).
fn strong(outcome: &str) -> Option<TaskOutcome> {
    TaskOutcome::parse(outcome).filter(|o| o.is_strong())
}

/// Strong-outcome counts of one stratum.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StratumCounts {
    /// `new_task` (the person's session) or `subagent` (a spawn): main and
    /// spawn draws are never pooled.
    pub relation: String,
    pub kind: String,
    pub band: &'static str,
    pub scope: String,
    pub drawn: usize,
    pub drawn_bad: usize,
    pub undrawn: usize,
    pub undrawn_bad: usize,
    /// The earliest drawn task's start (RFC 3339), for the projection.
    pub first_drawn_at: Option<String>,
}

/// The short relation prefix a stratum label starts with: `main` for the
/// person's session, `sub` for a subagent spawn.
pub fn relation_prefix(relation: &str) -> &'static str {
    if relation == "subagent" {
        "sub"
    } else {
        "main"
    }
}

impl StratumCounts {
    pub fn label(&self) -> String {
        format!(
            "{} {} {} {}",
            relation_prefix(&self.relation),
            self.kind,
            self.band,
            self.scope
        )
    }
}

fn rate(bad: usize, total: usize) -> Option<f64> {
    (total > 0).then(|| bad as f64 / total as f64)
}

pub fn drawn_rate(s: &StratumCounts) -> Option<f64> {
    rate(s.drawn_bad, s.drawn)
}

pub fn undrawn_rate(s: &StratumCounts) -> Option<f64> {
    rate(s.undrawn_bad, s.undrawn)
}

/// Per-stratum counts of tasks with strong outcomes: busiest stratum
/// (most drawn, then most tasks, then label) first.
pub fn strata(tasks: &[ExplorationTask]) -> Vec<StratumCounts> {
    let mut by_key: BTreeMap<(String, String, &'static str, String), StratumCounts> =
        BTreeMap::new();
    for task in tasks {
        let Some(outcome) = strong(&task.outcome) else {
            continue;
        };
        if !(task.drawn || task.undrawn) {
            continue;
        }
        let bad = outcome == TaskOutcome::Corrected;
        let entry = by_key
            .entry((
                task.relation.clone(),
                task.kind.clone(),
                band(task.difficulty),
                task.scope.clone(),
            ))
            .or_insert_with(|| StratumCounts {
                relation: task.relation.clone(),
                kind: task.kind.clone(),
                band: band(task.difficulty),
                scope: task.scope.clone(),
                drawn: 0,
                drawn_bad: 0,
                undrawn: 0,
                undrawn_bad: 0,
                first_drawn_at: None,
            });
        if task.drawn {
            entry.drawn += 1;
            entry.drawn_bad += usize::from(bad);
            if entry
                .first_drawn_at
                .as_ref()
                .is_none_or(|first| task.started_at < *first)
            {
                entry.first_drawn_at = Some(task.started_at.clone());
            }
        } else {
            entry.undrawn += 1;
            entry.undrawn_bad += usize::from(bad);
        }
    }
    let mut out: Vec<StratumCounts> = by_key.into_values().collect();
    out.sort_by(|a, b| {
        (b.drawn, b.drawn + b.undrawn)
            .cmp(&(a.drawn, a.drawn + a.undrawn))
            .then_with(|| a.label().cmp(&b.label()))
    });
    out
}

/// What the stop rule saw: drawn tasks against the undrawn tasks of the
/// strata they fall in.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StopCheck {
    pub drawn: usize,
    pub drawn_bad: usize,
    pub undrawn: usize,
    pub undrawn_bad: usize,
    pub drawn_rate: Option<f64>,
    pub undrawn_rate: Option<f64>,
    pub stop: bool,
}

/// The stop rule: once at least [`MIN_DRAWN`] drawn tasks have strong
/// outcomes, stop when their correction-or-failure rate exceeds that of
/// undrawn tasks in the same strata by more than [`STOP_MARGIN`].
pub fn stop_check(strata: &[StratumCounts]) -> StopCheck {
    let with_draws = strata.iter().filter(|s| s.drawn > 0);
    let (drawn, drawn_bad, undrawn, undrawn_bad) =
        with_draws.fold((0, 0, 0, 0), |(d, db, u, ub), s| {
            (
                d + s.drawn,
                db + s.drawn_bad,
                u + s.undrawn,
                ub + s.undrawn_bad,
            )
        });
    let drawn_rate = rate(drawn_bad, drawn);
    let undrawn_rate = rate(undrawn_bad, undrawn);
    let stop = match (drawn_rate, undrawn_rate) {
        (Some(d), Some(u)) => drawn >= MIN_DRAWN && d - u > STOP_MARGIN,
        _ => false,
    };
    StopCheck {
        drawn,
        drawn_bad,
        undrawn,
        undrawn_bad,
        drawn_rate,
        undrawn_rate,
        stop,
    }
}

/// When a stratum is projected to reach [`MIN_DRAWN`] drawn tasks, at the
/// rate it has drawn them since its first (at least one day).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Projection {
    /// No draw yet: nothing to project from.
    NoDraws,
    Reached,
    /// `date` is `YYYY-MM-DD`; `drawn` is how far the stratum is.
    On {
        date: String,
        drawn: usize,
    },
}

impl Projection {
    /// How far the stratum is from the [`MIN_DRAWN`] drawn tasks the stop
    /// rule and the learner wait for, always with the count.
    pub fn render(&self) -> String {
        match self {
            Projection::NoDraws => format!("no projection: 0 of {MIN_DRAWN} drawn tasks"),
            Projection::Reached => format!("{MIN_DRAWN} drawn reached"),
            Projection::On { date, drawn } => {
                format!("{drawn} of {MIN_DRAWN} drawn tasks, {MIN_DRAWN} projected {date}")
            }
        }
    }
}

pub fn project(s: &StratumCounts, now: DateTime<Utc>) -> Projection {
    if s.drawn == 0 {
        return Projection::NoDraws;
    }
    if s.drawn >= MIN_DRAWN {
        return Projection::Reached;
    }
    let first = s
        .first_drawn_at
        .as_deref()
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map_or(now, |at| at.with_timezone(&Utc));
    let days = ((now - first).num_seconds() as f64 / 86_400.0).max(1.0);
    let per_day = s.drawn as f64 / days;
    let remaining = (MIN_DRAWN - s.drawn) as f64;
    let seconds = (remaining / per_day * 86_400.0).ceil() as i64;
    let date = (now + Duration::seconds(seconds))
        .format("%Y-%m-%d")
        .to_string();
    Projection::On {
        date,
        drawn: s.drawn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(drawn: bool, bad: bool) -> ExplorationTask {
        ExplorationTask {
            relation: "new_task".into(),
            kind: "edit".into(),
            difficulty: 3,
            scope: "local".into(),
            outcome: if bad {
                "corrected"
            } else {
                "completed_verified"
            }
            .into(),
            started_at: "2026-10-01T00:00:00Z".into(),
            drawn,
            undrawn: !drawn,
        }
    }

    /// `drawn_bad` of 20 drawn tasks corrected, 0 of 40 undrawn.
    fn fixture(drawn_bad: usize) -> Vec<ExplorationTask> {
        let mut tasks: Vec<_> = (0..20).map(|i| task(true, i < drawn_bad)).collect();
        tasks.extend((0..40).map(|_| task(false, false)));
        tasks
    }

    #[test]
    fn bands_split_difficulty() {
        assert_eq!(
            [1, 2, 3, 4, 5].map(band),
            ["d1-2", "d1-2", "d3", "d4-5", "d4-5"]
        );
    }

    #[test]
    fn the_stop_fires_above_fifteen_points_and_not_at_or_below() {
        // 4 of 20 = 20 points over a clean undrawn arm: stop.
        let over = stop_check(&strata(&fixture(4)));
        assert!(over.stop, "{over:?}");
        assert_eq!((over.drawn, over.drawn_bad, over.undrawn), (20, 4, 40));
        // 3 of 20 = exactly 15 points: not more than 15.
        assert!(!stop_check(&strata(&fixture(3))).stop);
        assert!(!stop_check(&strata(&fixture(0))).stop);
    }

    #[test]
    fn the_stop_waits_for_twenty_drawn_tasks_with_strong_outcomes() {
        let mut tasks = fixture(4);
        tasks.remove(0);
        assert!(!stop_check(&strata(&tasks)).stop, "19 drawn");
        // An unknown outcome is no evidence.
        let mut unknown = task(true, true);
        unknown.outcome = "unknown".into();
        tasks.push(unknown);
        assert!(!stop_check(&strata(&tasks)).stop, "20 drawn, one unknown");
    }

    #[test]
    fn an_answered_outcome_is_no_evidence_for_the_stop() {
        // 19 drawn tasks with strong outcomes, 4 of them corrected: 21
        // points over the clean undrawn arm, but one short of twenty.
        let mut tasks = fixture(4);
        tasks.remove(19);
        assert!(!stop_check(&strata(&tasks)).stop, "19 drawn");
        // A twentieth, answered: were it counted, the stop would fire.
        let mut answered = task(true, false);
        answered.outcome = "answered".into();
        tasks.push(answered);
        let check = stop_check(&strata(&tasks));
        assert!(!check.stop, "{check:?}");
    }

    #[test]
    fn the_comparison_is_inside_the_strata_the_draws_fall_in() {
        let mut tasks = fixture(4);
        // A very bad undrawn stratum with no draws must not hide the harm.
        for _ in 0..40 {
            let mut other = task(false, true);
            other.kind = "debug".into();
            tasks.push(other);
        }
        assert!(stop_check(&strata(&tasks)).stop);
    }

    #[test]
    fn no_undrawn_tasks_means_nothing_to_compare() {
        let tasks: Vec<_> = (0..20).map(|_| task(true, true)).collect();
        assert!(!stop_check(&strata(&tasks)).stop);
    }

    #[test]
    fn a_held_out_or_gate_blocked_task_is_neither_drawn_nor_undrawn() {
        let mut neither = task(false, true);
        neither.undrawn = false;
        assert!(strata(&[neither]).is_empty());
    }

    #[test]
    fn projection_reads_no_draws_a_date_or_reached() {
        let now = DateTime::parse_from_rfc3339("2026-10-11T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut s = strata(&fixture(0)).remove(0);
        assert_eq!(project(&s, now), Projection::Reached);
        // 5 drawn in 10 days: 15 more take 30 days.
        s.drawn = 5;
        let on = project(&s, now);
        assert_eq!(
            on,
            Projection::On {
                date: "2026-11-10".into(),
                drawn: 5
            }
        );
        assert_eq!(on.render(), "5 of 20 drawn tasks, 20 projected 2026-11-10");
        s.drawn = 0;
        s.first_drawn_at = None;
        let none = project(&s, now);
        assert_eq!(none, Projection::NoDraws);
        assert_eq!(none.render(), "no projection: 0 of 20 drawn tasks");
    }

    #[test]
    fn main_and_subagent_draws_are_separate_strata() {
        let mut tasks = fixture(4);
        // The same class drawn by spawns, all clean: a separate stratum,
        // labelled `sub`, and the main stratum's rate is untouched.
        for _ in 0..20 {
            let mut spawn = task(true, false);
            spawn.relation = "subagent".into();
            tasks.push(spawn);
        }
        for _ in 0..40 {
            let mut spawn = task(false, false);
            spawn.relation = "subagent".into();
            tasks.push(spawn);
        }
        let strata = strata(&tasks);
        assert_eq!(strata.len(), 2);
        let labels: Vec<String> = strata.iter().map(StratumCounts::label).collect();
        assert_eq!(labels, ["main edit d3 local", "sub edit d3 local"]);
        assert_eq!(drawn_rate(&strata[0]), Some(0.2));
        assert_eq!(drawn_rate(&strata[1]), Some(0.0));
        // The stop rule stays one rule over every stratum with draws: 4 of
        // 40 drawn against 0 of 80 undrawn is 10 points, under the margin.
        assert!(!stop_check(&strata).stop);
    }
}
