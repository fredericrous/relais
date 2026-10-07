//! `relais report`'s "session routing" section (plan §5, "Spend
//! headline"): the total cost per completed task in routed sessions
//! against held-out ones, priced from `[pricing]` at read time, with a
//! cluster-bootstrap interval over sessions. Every figure is an
//! API-equivalent estimate; an unpriced model makes its arm's cost
//! unknown, never zero (SPEC §11). Pure.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::outcome::TaskOutcome;
use super::stats::cluster_bootstrap_ratio;
use crate::ledger::{RouterUsageRow, RouterWindow};
use crate::money::MicroUsd;
use crate::orchestration::{price, CacheWrites, PriceTable, Speed, UsageRecord};

/// The bootstrap's fixed seed: the same ledger always prints the same
/// interval.
pub const BOOTSTRAP_SEED: u64 = 0x7265_6c61_6973_0030;
pub const BOOTSTRAP_RESAMPLES: usize = 1000;

/// What the figures are.
pub const BASIS: &str = "API-equivalent estimate";

/// One arm of the comparison.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Arm {
    /// `routed` (a decision was applied), `held_out` (the session was
    /// drawn into the hold-out) or `shadow` (decided, nothing applied).
    pub arm: &'static str,
    pub sessions: usize,
    pub tasks: usize,
    pub completed: usize,
    /// All of the arm's router usage, completed or not. `None` when any
    /// of it is unpriced.
    pub cost: Option<MicroUsd>,
    pub cost_per_completed_task: Option<MicroUsd>,
    /// The 95% cluster-bootstrap interval of `cost_per_completed_task`.
    pub interval: Option<[MicroUsd; 2]>,
    pub unpriced_models: Vec<String>,
}

/// The classifier's own spend, reported on its own line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Classifier {
    pub calls: usize,
    pub cost: Option<MicroUsd>,
    pub unpriced_models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionRoutingReport {
    pub basis: &'static str,
    pub pricing_version: String,
    pub arms: Vec<Arm>,
    pub tasks: usize,
    pub completed: usize,
    pub corrected: usize,
    pub unknown: usize,
    pub unknown_rate: Option<f64>,
    pub classifier: Classifier,
    /// Cache-write tokens on the first request after a model switch,
    /// within one session's main thread or one subagent.
    pub cache_rebuild_tokens: u64,
}

fn priced(row: &RouterUsageRow, pricing: &PriceTable) -> Option<MicroUsd> {
    // The rebuild is priced at the 1-hour write rate (S0: the person's
    // cache writes are 1-hour writes).
    let record = UsageRecord {
        message_id: String::new(),
        model: row.model.clone(),
        speed: Speed::Standard,
        input_tokens: row.input_tokens,
        output_tokens: row.output_tokens,
        cache_read_input_tokens: row.cache_read_input_tokens,
        cache_writes: CacheWrites {
            ephemeral_5m_input_tokens: 0,
            ephemeral_1h_input_tokens: row.cache_creation_input_tokens,
        },
        timestamp: row.at.clone(),
    };
    price(&record, pricing).cost
}

/// Sum a set of usage rows: the total when every one prices, and the
/// models that do not.
fn total<'a>(
    rows: impl Iterator<Item = &'a RouterUsageRow>,
    pricing: &PriceTable,
) -> (Option<MicroUsd>, Vec<String>) {
    let mut sum = MicroUsd::ZERO;
    let mut unpriced = BTreeSet::new();
    for row in rows {
        match priced(row, pricing) {
            Some(cost) => sum = sum.saturating_add(cost),
            None => {
                unpriced.insert(row.model.clone());
            }
        }
    }
    let unpriced: Vec<String> = unpriced.into_iter().collect();
    (unpriced.is_empty().then_some(sum), unpriced)
}

/// Cache writes on the first step after the model changed, per thread.
fn cache_rebuild_tokens(usage: &[(String, RouterUsageRow)]) -> u64 {
    let mut last: BTreeMap<(&str, Option<&str>), &str> = BTreeMap::new();
    let mut tokens = 0u64;
    for (session, row) in usage.iter().filter(|(_, row)| row.source == "step") {
        let thread = (session.as_str(), row.agent_id.as_deref());
        if let Some(previous) = last.insert(thread, row.model.as_str()) {
            if previous != row.model {
                tokens = tokens.saturating_add(row.cache_creation_input_tokens);
            }
        }
    }
    tokens
}

/// The section, from the window's rows.
pub fn session_routing(window: &RouterWindow, pricing: &PriceTable) -> SessionRoutingReport {
    // Each session's arm: held out wins, then routed, else shadow.
    let mut arm_of: BTreeMap<&str, &'static str> = BTreeMap::new();
    for (session, _) in &window.tasks {
        arm_of.entry(session.as_str()).or_insert("shadow");
    }
    for (session, holdout, applied) in &window.decisions {
        let arm = arm_of.entry(session.as_str()).or_insert("shadow");
        if *holdout {
            *arm = "held_out";
        } else if *applied && *arm != "held_out" {
            *arm = "routed";
        }
    }

    let outcome = |name: &str| TaskOutcome::parse(name).unwrap_or(TaskOutcome::Unknown);
    let arms = ["routed", "held_out", "shadow"]
        .into_iter()
        .map(|name| {
            let sessions: Vec<&str> = arm_of
                .iter()
                .filter(|(_, arm)| **arm == name)
                .map(|(session, _)| *session)
                .collect();
            let tasks: Vec<_> = window
                .tasks
                .iter()
                .filter(|(session, _)| sessions.contains(&session.as_str()))
                .collect();
            let completed = tasks
                .iter()
                .filter(|(_, task)| outcome(&task.outcome).is_completed())
                .count();
            let usage = || {
                window
                    .usage
                    .iter()
                    .filter(|(session, _)| sessions.contains(&session.as_str()))
            };
            let (cost, unpriced_models) = total(usage().map(|(_, row)| row), pricing);
            let per_task = cost
                .filter(|_| completed > 0)
                .map(|cost| MicroUsd::from_micros(cost.to_micros() / completed as i64));
            let interval = cost.and_then(|_| {
                let clusters: Vec<(f64, f64)> = sessions
                    .iter()
                    .map(|session| {
                        let (cost, _) = total(
                            usage().filter(|(s, _)| s == session).map(|(_, row)| row),
                            pricing,
                        );
                        let done = tasks
                            .iter()
                            .filter(|(s, task)| {
                                s == session && outcome(&task.outcome).is_completed()
                            })
                            .count();
                        (cost.map_or(0.0, |c| c.to_micros() as f64), done as f64)
                    })
                    .collect();
                cluster_bootstrap_ratio(&clusters, BOOTSTRAP_SEED, BOOTSTRAP_RESAMPLES, 0.05).map(
                    |(lo, hi)| {
                        [
                            MicroUsd::from_micros(lo.round() as i64),
                            MicroUsd::from_micros(hi.round() as i64),
                        ]
                    },
                )
            });
            Arm {
                arm: name,
                sessions: sessions.len(),
                tasks: tasks.len(),
                completed,
                cost,
                cost_per_completed_task: per_task,
                interval: per_task.and(interval),
                unpriced_models,
            }
        })
        .collect();

    let count = |wanted: fn(TaskOutcome) -> bool| {
        window
            .tasks
            .iter()
            .filter(|(_, task)| wanted(outcome(&task.outcome)))
            .count()
    };
    let tasks = window.tasks.len();
    let unknown = count(|o| o == TaskOutcome::Unknown);
    let classifier_rows: Vec<&RouterUsageRow> = window
        .usage
        .iter()
        .map(|(_, row)| row)
        .filter(|row| row.source == "classifier")
        .collect();
    let (classifier_cost, classifier_unpriced) = total(classifier_rows.iter().copied(), pricing);
    SessionRoutingReport {
        basis: BASIS,
        pricing_version: pricing.version.clone(),
        arms,
        tasks,
        completed: count(TaskOutcome::is_completed),
        corrected: count(|o| o == TaskOutcome::Corrected),
        unknown,
        unknown_rate: (tasks > 0).then(|| unknown as f64 / tasks as f64),
        classifier: Classifier {
            calls: classifier_rows.len(),
            cost: classifier_cost,
            unpriced_models: classifier_unpriced,
        },
        cache_rebuild_tokens: cache_rebuild_tokens(&window.usage),
    }
}

fn unknown_cost(unpriced: &[String]) -> String {
    format!(
        "unknown (no [pricing.models] entry for {})",
        unpriced.join(", ")
    )
}

impl SessionRoutingReport {
    /// The section as `relais report` prints it.
    pub fn render(&self) -> String {
        let version = if self.pricing_version.is_empty() {
            "no [pricing] table".to_string()
        } else {
            format!("[pricing] {}", self.pricing_version)
        };
        let mut out = format!("session routing ({}, {version}):\n", self.basis);
        for arm in &self.arms {
            let label = match arm.arm {
                "held_out" => "held out",
                other => other,
            };
            let figure = match (arm.cost_per_completed_task, arm.cost) {
                (_, None) => format!(
                    "cost per completed task {}",
                    unknown_cost(&arm.unpriced_models)
                ),
                (None, Some(_)) => "cost per completed task: no completed task".to_string(),
                (Some(per_task), Some(_)) => match arm.interval {
                    Some([lo, hi]) => {
                        format!("cost per completed task {per_task} (95% interval {lo} to {hi})")
                    }
                    None => format!("cost per completed task {per_task}"),
                },
            };
            out.push_str(&format!(
                "  {label}: {} session(s), {} task(s), {} completed; {figure}\n",
                arm.sessions, arm.tasks, arm.completed
            ));
        }
        let rate = self
            .unknown_rate
            .map_or_else(|| "n/a".to_string(), |r| format!("{:.0}%", r * 100.0));
        out.push_str(&format!(
            "  outcomes: {} completed, {} corrected, {} unknown (unknown rate {rate})\n",
            self.completed, self.corrected, self.unknown
        ));
        let classifier = match self.classifier.cost {
            Some(cost) => cost.to_string(),
            None => unknown_cost(&self.classifier.unpriced_models),
        };
        out.push_str(&format!(
            "  classifier: {} call(s), {classifier}\n",
            self.classifier.calls
        ));
        out.push_str(&format!(
            "  cache rebuilt after a model switch: {} token(s) written\n",
            self.cache_rebuild_tokens
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::RouterTaskRow;
    use crate::orchestration::ModelPrice;

    fn pricing() -> PriceTable {
        PriceTable {
            version: "2026-10".into(),
            models: vec![ModelPrice {
                ids: vec!["claude-haiku-5-5".into()],
                input: 1_000_000,
                output: 5_000_000,
                cache_read: 100_000,
                cache_write_5m: 1_250_000,
                cache_write_1h: 2_000_000,
                fast_input: None,
                fast_output: None,
            }],
        }
    }

    fn task(id: &str, outcome: &str) -> RouterTaskRow {
        RouterTaskRow {
            task_id: id.into(),
            agent_id: None,
            started_at: "2026-10-07T10:00:00Z".into(),
            ended_at: None,
            class: None,
            outcome: outcome.into(),
            outcome_rank: 0,
            inferred: vec![],
            escalations: 0,
            exhausted: false,
            turns: 1,
            explicit_quote: None,
        }
    }

    fn usage(task: &str, step: u32, model: &str, source: &str, cache_write: u64) -> RouterUsageRow {
        RouterUsageRow {
            task_id: task.into(),
            turn_id: Some("u".into()),
            step,
            agent_id: None,
            source: source.into(),
            model: model.into(),
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: cache_write,
            at: format!("2026-10-07T10:00:0{step}Z"),
        }
    }

    fn window() -> RouterWindow {
        RouterWindow {
            tasks: vec![
                ("routed-1".into(), task("a", "completed_verified")),
                ("routed-1".into(), task("b", "corrected")),
                ("held-1".into(), task("c", "completed_accepted")),
                ("shadow-1".into(), task("d", "unknown")),
            ],
            usage: vec![
                (
                    "routed-1".into(),
                    usage("a", 0, "claude-haiku-5-5", "step", 0),
                ),
                (
                    "routed-1".into(),
                    usage("a", 1, "claude-haiku-5-5", "classifier", 0),
                ),
                (
                    "held-1".into(),
                    usage("c", 0, "claude-haiku-5-5", "step", 0),
                ),
                (
                    "held-1".into(),
                    usage("c", 1, "claude-opus-5-5", "step", 500),
                ),
            ],
            decisions: vec![
                ("routed-1".into(), false, true),
                ("held-1".into(), true, false),
                ("shadow-1".into(), false, false),
            ],
        }
    }

    #[test]
    fn a_priced_arm_has_a_cost_per_completed_task_and_an_unpriced_one_is_unknown() {
        let report = session_routing(&window(), &pricing());
        let routed = &report.arms[0];
        assert_eq!(
            (routed.arm, routed.sessions, routed.tasks),
            ("routed", 1, 2)
        );
        assert_eq!(routed.completed, 1);
        // Two rows of a million input tokens at $1/Mtok, the classifier
        // call included, over one completed task.
        assert_eq!(
            routed.cost_per_completed_task,
            Some(MicroUsd::from_micros(2_000_000))
        );
        assert!(routed.interval.is_some());
        let held = &report.arms[1];
        assert_eq!(held.arm, "held_out");
        assert_eq!(held.cost, None);
        assert_eq!(held.unpriced_models, vec!["claude-opus-5-5".to_string()]);
        let text = report.render();
        assert!(text.contains("API-equivalent estimate"), "{text}");
        assert!(
            text.contains("held out: 1 session(s), 1 task(s), 1 completed; cost per completed task unknown (no [pricing.models] entry for claude-opus-5-5)"),
            "{text}"
        );
        assert!(
            text.contains("routed: 1 session(s), 2 task(s), 1 completed; cost per completed task $2 (95% interval"),
            "{text}"
        );
    }

    #[test]
    fn counts_unknown_rate_classifier_and_rebuilds() {
        let report = session_routing(&window(), &pricing());
        assert_eq!(
            (
                report.tasks,
                report.completed,
                report.corrected,
                report.unknown
            ),
            (4, 2, 1, 1)
        );
        assert_eq!(report.unknown_rate, Some(0.25));
        assert_eq!(report.classifier.calls, 1);
        assert_eq!(
            report.classifier.cost,
            Some(MicroUsd::from_micros(1_000_000))
        );
        // haiku → opus in the held-out session wrote 500 tokens.
        assert_eq!(report.cache_rebuild_tokens, 500);
        assert_eq!(report.arms[2].arm, "shadow");
        assert_eq!(report.arms[2].sessions, 1);
    }

    #[test]
    fn the_interval_is_the_same_every_time() {
        let a = session_routing(&window(), &pricing());
        let b = session_routing(&window(), &pricing());
        assert_eq!(a, b);
    }
}
