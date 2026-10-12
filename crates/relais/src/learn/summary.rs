//! What `relais report` says about learning (SPEC §17): the last cycle, the
//! labelled examples, the active artifact and the share of dispatches it
//! routed, the last rollback. Nothing learned is a sentence that says why,
//! never an empty section. Every rendered row fits in 80 columns.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::ledger::{LearningAction, LearningCycleRow, LearningSource, Ledger, LedgerError};

use super::dataset::CounterfactualGap;
use super::evaluate::{EvalReport, SweepPoint};
use super::registry::Registry;

const WIDTH: usize = 80;

/// How many sweep rows the section shows: the configured floor and its
/// two neighbours either side.
const SWEEP_ROWS: usize = 5;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastCycle {
    pub at: String,
    pub action: LearningAction,
    pub failed_gates: Vec<String>,
    pub error: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActiveArtifact {
    pub artifact_id: String,
    pub active_since: Option<String>,
    /// Runs with a recorded route since then.
    pub runs_since: u64,
    /// Of those, the runs the learned artifact routed.
    pub routed_by_it: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastRollback {
    pub at: String,
    pub source: LearningSource,
    pub artifact_id: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearningReport {
    /// Present when nothing has been learned: the sentence, starting
    /// "Nothing learned yet, because".
    pub nothing_learned: Option<String>,
    pub last_cycle: Option<LastCycle>,
    pub examples_by_class: BTreeMap<String, u64>,
    pub examples_by_source: BTreeMap<String, u64>,
    /// The latest cycle's dataset by origin (`live`, `replay`); empty when
    /// its artifact predates origins.
    pub examples_by_origin: BTreeMap<String, usize>,
    /// The latest cycle's counterfactual gap; `null` when unavailable.
    pub counterfactual_gap: Option<CounterfactualGap>,
    /// The latest cycle's floor sweep; `null`, never `[]`, when the artifact
    /// is missing, unreadable or predates the sweep (`floor_sweep_note`
    /// says which).
    pub floor_sweep: Option<Vec<SweepPoint>>,
    pub floor_sweep_note: Option<String>,
    pub active_artifact: Option<ActiveArtifact>,
    pub last_rollback: Option<LastRollback>,
}

/// The latest cycle's stored evaluation, read leniently: `evaluated()`
/// refuses failed gates and other versions, and this is a report, not a
/// promotion.
enum LatestEvaluation {
    Report(Box<EvalReport>),
    Unavailable(String),
}

fn latest_evaluation(
    registry: Option<&Registry>,
    last: Option<&LearningCycleRow>,
) -> LatestEvaluation {
    let Some(registry) = registry else {
        return LatestEvaluation::Unavailable("the registry did not open".into());
    };
    let Some(id) = last.and_then(|row| row.artifact_id.as_deref()) else {
        return LatestEvaluation::Unavailable("no cycle has written an artifact yet".into());
    };
    let artifact = match registry.load(id) {
        Ok(artifact) => artifact,
        Err(e) => return LatestEvaluation::Unavailable(format!("artifact {id}: {e}")),
    };
    let Some(evaluation) = artifact.evaluation else {
        return LatestEvaluation::Unavailable(format!("artifact {id} carries no evaluation"));
    };
    match serde_json::from_value::<EvalReport>(evaluation) {
        Ok(report) => LatestEvaluation::Report(Box::new(report)),
        Err(e) => LatestEvaluation::Unavailable(format!("artifact {id}: {e}")),
    }
}

fn minute(stamp: &str) -> String {
    stamp
        .get(..16)
        .map(|head| head.replace('T', " "))
        .unwrap_or_else(|| stamp.to_string())
}

/// The learning section, from the ledger and (when it opens) the registry.
pub fn learning_report(
    ledger: &Ledger,
    registry: Option<&Registry>,
) -> Result<LearningReport, LedgerError> {
    // The last cycle a CYCLE ran: a person's rollback row is not one.
    let last_cycle_row = ledger.last_cycle_started()?;
    let dataset_row = ledger.last_learning_dataset_row()?;
    let rollback = ledger.learning_rollbacks()?.into_iter().next();
    let active = match registry.and_then(|registry| registry.active().ok().flatten()) {
        Some(artifact) => {
            let since = registry.and_then(Registry::active_changed_at);
            let (runs_since, routed_by_it) = match &since {
                Some(since) => ledger.learned_routing_share_since(since)?,
                None => (0, 0),
            };
            Some(ActiveArtifact {
                artifact_id: artifact.artifact_id,
                active_since: since,
                runs_since,
                routed_by_it,
            })
        }
        None => None,
    };
    let nothing_learned = match &active {
        Some(_) => None,
        None => Some(reason_nothing_learned(last_cycle_row.as_ref())),
    };
    let (examples_by_origin, counterfactual_gap, floor_sweep, floor_sweep_note) =
        match latest_evaluation(registry, last_cycle_row.as_ref()) {
            LatestEvaluation::Report(report) if report.floor_sweep.is_empty() => (
                report.examples_by_origin.clone(),
                report.counterfactual_gap.clone(),
                None,
                Some("not in this artifact; run relais learn cycle".to_string()),
            ),
            LatestEvaluation::Report(report) => (
                report.examples_by_origin.clone(),
                report.counterfactual_gap.clone(),
                Some(report.floor_sweep.clone()),
                None,
            ),
            LatestEvaluation::Unavailable(why) => (
                BTreeMap::new(),
                None,
                None,
                Some(format!("unavailable ({why})")),
            ),
        };
    Ok(LearningReport {
        nothing_learned,
        examples_by_origin,
        counterfactual_gap,
        floor_sweep,
        floor_sweep_note,
        last_cycle: last_cycle_row.map(|row| LastCycle {
            at: row.started_at,
            action: row.action,
            failed_gates: row
                .evaluate
                .map(|verdict| verdict.failed_gates)
                .unwrap_or_default(),
            error: row.error,
            note: row.note,
        }),
        examples_by_class: dataset_row
            .as_ref()
            .map(|row| row.examples_by_class.clone())
            .unwrap_or_default(),
        examples_by_source: dataset_row
            .map(|row| row.examples_by_source)
            .unwrap_or_default(),
        active_artifact: active,
        last_rollback: rollback.map(|row| LastRollback {
            at: row.ended_at.unwrap_or(row.started_at),
            source: row.source,
            artifact_id: row.artifact_id,
            note: row.note,
        }),
    })
}

fn reason_nothing_learned(last: Option<&LearningCycleRow>) -> String {
    let because = match last {
        None => "no learning cycle has run yet (one starts at the beginning and the end of a \
                 session, at most once a day)"
            .to_string(),
        Some(row) => match row.action {
            LearningAction::AwaitingPromotion => format!(
                "the last cycle's artifact {} passed its gates and waits for `relais promote`",
                row.artifact_id.as_deref().unwrap_or("?")
            ),
            LearningAction::Error => format!(
                "the last cycle stopped on an error: {}",
                row.error.as_deref().unwrap_or("unrecorded")
            ),
            LearningAction::Interrupted => {
                "the last cycle was interrupted before it finished".to_string()
            }
            LearningAction::None | LearningAction::Promoted | LearningAction::RolledBack => {
                match row.evaluate.as_ref().and_then(|v| v.failed_gates.first()) {
                    Some(gate) => format!(
                        "the last cycle ({}) trained nothing promotable: {gate}",
                        minute(&row.started_at)
                    ),
                    None => format!(
                        "the last cycle ({}) had nothing to promote",
                        minute(&row.started_at)
                    ),
                }
            }
        },
    };
    format!("Nothing learned yet, because {because}.")
}

/// The [`SWEEP_ROWS`] points around the configured floor: it and its two
/// neighbours either side, shifted at the ends so five are shown when
/// there are five.
fn sweep_window(points: &[SweepPoint]) -> &[SweepPoint] {
    let at = points
        .iter()
        .position(|point| point.configured)
        .unwrap_or(0);
    let start = at.saturating_sub(SWEEP_ROWS / 2);
    let end = (start + SWEEP_ROWS).min(points.len());
    let start = end.saturating_sub(SWEEP_ROWS);
    &points[start..end]
}

/// One sweep row; the configured floor is named, never marked alone.
fn sweep_row(point: &SweepPoint) -> String {
    let percent = |rate: Option<f64>| {
        rate.map_or_else(|| "n/a".to_string(), |r| format!("{:.0}%", r * 100.0))
    };
    format!(
        "floor {:.2}{}  accepted {}  abstain {}  {}  n={}",
        point.floor,
        if point.configured { " configured" } else { "" },
        percent(point.acceptance_rate),
        percent(Some(point.abstention_rate)),
        match super::evaluate::micros_of(point.mean_cost_micros) {
            Some(cost) => format!("{cost}/task"),
            None => "cost n/a".to_string(),
        },
        point.supported_test_records
    )
}

/// `text` wrapped to the section width, every line after the first indented.
fn wrapped(first: &str, indent: &str, text: &str) -> String {
    let mut out = String::new();
    let mut line = first.to_string();
    let mut fresh = true;
    for word in text.split_whitespace() {
        // A word longer than the row is cut: the rule is "within 80
        // columns", and a path or a hash must not break it.
        // It lands after `first` or after `indent`: cut it for the wider.
        let prefix = first.chars().count().max(indent.chars().count());
        let word: String = if prefix + word.chars().count() > WIDTH {
            word.chars()
                .take(WIDTH.saturating_sub(prefix + 1))
                .collect::<String>()
                + "…"
        } else {
            word.to_string()
        };
        let needed = line.chars().count() + usize::from(!fresh) + word.chars().count();
        if needed > WIDTH && !fresh {
            out.push_str(&line);
            out.push('\n');
            line = indent.to_string();
            fresh = true;
        }
        if !fresh {
            line.push(' ');
        }
        line.push_str(&word);
        fresh = false;
    }
    out.push_str(&line);
    out.push('\n');
    out
}

impl LearningReport {
    pub fn render(&self) -> String {
        let mut out = String::from("\nlearning:\n");
        if let Some(sentence) = &self.nothing_learned {
            out.push_str(&wrapped("  ", "  ", sentence));
        }
        match &self.last_cycle {
            Some(cycle) => {
                out.push_str(&wrapped(
                    "  last cycle: ",
                    "    ",
                    &format!("{} {}", minute(&cycle.at), cycle.action.as_str()),
                ));
                for gate in &cycle.failed_gates {
                    out.push_str(&wrapped("    gate: ", "      ", gate));
                }
                if let Some(error) = &cycle.error {
                    out.push_str(&wrapped("    error: ", "      ", error));
                }
            }
            None => out.push_str("  last cycle: none yet\n"),
        }
        if self.examples_by_class.is_empty() {
            out.push_str("  labelled examples: none yet\n");
        } else {
            let join = |counts: &BTreeMap<String, u64>| {
                if counts.is_empty() {
                    return "none".to_string();
                }
                counts
                    .iter()
                    .map(|(name, count)| format!("{name} {count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            out.push_str(&wrapped(
                "  labelled examples by class: ",
                "    ",
                &join(&self.examples_by_class),
            ));
            out.push_str(&wrapped(
                "  labelled examples by source: ",
                "    ",
                &join(&self.examples_by_source),
            ));
        }
        if !self.examples_by_origin.is_empty() {
            out.push_str(&wrapped(
                "  examples by origin: ",
                "    ",
                &self
                    .examples_by_origin
                    .iter()
                    .map(|(origin, count)| format!("{origin} {count}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        if let Some(gap) = &self.counterfactual_gap {
            for line in gap.render_lines() {
                out.push_str(&wrapped("  ", "    ", &line));
            }
        }
        match &self.floor_sweep {
            Some(points) => {
                out.push_str("  floor sweep (test split; the configured floor is named):\n");
                for point in sweep_window(points) {
                    out.push_str(&wrapped("    ", "      ", &sweep_row(point)));
                }
            }
            None => {
                out.push_str(&wrapped(
                    "  floor sweep: ",
                    "    ",
                    self.floor_sweep_note.as_deref().unwrap_or("unavailable"),
                ));
            }
        }
        match &self.active_artifact {
            Some(active) => {
                out.push_str(&wrapped(
                    "  active artifact: ",
                    "    ",
                    &format!(
                        "{} since {}",
                        active.artifact_id,
                        active
                            .active_since
                            .as_deref()
                            .map(minute)
                            .unwrap_or_else(|| "?".into())
                    ),
                ));
                let share = if active.runs_since == 0 {
                    "no dispatches since".to_string()
                } else {
                    format!(
                        "it routed {} of {} dispatches ({:.0}%)",
                        active.routed_by_it,
                        active.runs_since,
                        100.0 * active.routed_by_it as f64 / active.runs_since as f64
                    )
                };
                out.push_str(&wrapped("    ", "    ", &share));
            }
            None => out.push_str("  active artifact: none\n"),
        }
        match &self.last_rollback {
            Some(rollback) => {
                let by = match rollback.source {
                    LearningSource::Person => "by a person",
                    LearningSource::Cycle => "by the cycle",
                };
                out.push_str(&wrapped(
                    "  last rollback: ",
                    "    ",
                    &format!("{} {by}", minute(&rollback.at)),
                ));
                if let Some(note) = &rollback.note {
                    out.push_str(&wrapped("    ", "    ", note));
                }
            }
            None => out.push_str("  last rollback: none\n"),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> LearningReport {
        LearningReport {
            nothing_learned: Some(reason_nothing_learned(None)),
            last_cycle: Some(LastCycle {
                at: "2026-10-10T12:00:00Z".into(),
                action: LearningAction::None,
                failed_gates: vec![
                    "tier research has 3 training record(s), below the 5 required and then some \
                     very long words follow so that the row has to be wrapped onto another line \
                     of the report"
                        .into(),
                    "a_single_word_that_is_much_longer_than_the_whole_row_so_it_must_be_cut_to_keep_every_row_inside_the_width_rule".into(),
                ],
                error: None,
                note: None,
            }),
            examples_by_class: BTreeMap::from([("other".into(), 3)]),
            examples_by_source: BTreeMap::from([("runner".into(), 3)]),
            examples_by_origin: BTreeMap::from([("live".into(), 3), ("replay".into(), 1)]),
            counterfactual_gap: Some(CounterfactualGap {
                tasks: 14,
                estimate: None,
                basis: "no priced run at research/implementation".into(),
            }),
            floor_sweep: Some(sweep(0.75)),
            floor_sweep_note: None,
            active_artifact: None,
            last_rollback: None,
        }
    }

    /// Ten grid points with worst-case widths: 100% rates, a three-digit
    /// dollar cost and five-digit support.
    fn sweep(configured: f64) -> Vec<SweepPoint> {
        super::super::evaluate::sweep_floors(configured)
            .into_iter()
            .map(|floor| SweepPoint {
                floor,
                configured: (floor * 100.0).round() == (configured * 100.0).round(),
                supported_test_records: 99_999,
                acceptance_rate: Some(1.0),
                mean_cost_micros: Some(123_450_000),
                abstention_rate: 1.0,
            })
            .collect()
    }

    #[test]
    fn nothing_learned_is_a_sentence_that_says_why() {
        let text = report().render();
        assert!(
            text.contains("Nothing learned yet, because no learning cycle"),
            "{text}"
        );
    }

    #[test]
    fn every_row_stays_within_80_columns() {
        let mut report = report();
        report.counterfactual_gap = Some(CounterfactualGap {
            tasks: 99_999,
            estimate: Some(crate::money::MicroUsd::from_micros(123_456_789_012)),
            basis: "priced".into(),
        });
        let text = report.render();
        for line in text.lines() {
            assert!(line.chars().count() <= 80, "{line:?}");
        }
        assert!(text.contains("gate: "), "{text}");
        assert!(
            text.contains("examples by origin: live 3, replay 1"),
            "{text}"
        );
        assert!(
            text.contains("counterfactual gap: 99999 accepted task(s) lack a cheaper arm"),
            "{text}"
        );
        assert!(
            text.contains("next: relais dataset replay (estimated cost ~$123456.789012)"),
            "{text}"
        );
    }

    #[test]
    fn the_sweep_shows_at_most_five_rows_with_the_configured_floor_named() {
        let text = report().render();
        let rows: Vec<&str> = text
            .lines()
            .filter(|line| line.trim_start().starts_with("floor 0."))
            .collect();
        assert_eq!(rows.len(), 5, "{text}");
        assert_eq!(
            rows.iter().filter(|row| row.contains("configured")).count(),
            1,
            "{text}"
        );
        assert!(rows[2].contains("floor 0.75 configured"), "{text}");
        assert!(rows[0].contains("floor 0.65"), "{text}");
        assert!(rows[4].contains("floor 0.85"), "{text}");
        assert!(rows[2].contains("$123.45/task"), "{text}");
        // At the ends the window shifts, and five are still shown.
        let mut low = report();
        low.floor_sweep = Some(sweep(0.5));
        let text = low.render();
        let rows: Vec<&str> = text
            .lines()
            .filter(|line| line.trim_start().starts_with("floor 0."))
            .collect();
        assert_eq!(rows.len(), 5, "{text}");
        assert!(rows[0].contains("floor 0.50 configured"), "{text}");
        // The gap with no priced run says so, never `$0`.
        // The line wraps at 80 columns, so the two halves are checked apart.
        assert!(text.contains("(cost unknown: no priced run at"), "{text}");
        assert!(text.contains("research/implementation)"), "{text}");
        assert!(!text.contains("$0)"), "{text}");
    }

    #[test]
    fn a_missing_sweep_is_an_explicit_line_and_null_in_json() {
        let mut report = report();
        report.floor_sweep = None;
        report.floor_sweep_note = Some("not in this artifact; run relais learn cycle".into());
        let text = report.render();
        assert!(
            text.contains("floor sweep: not in this artifact; run relais learn cycle"),
            "{text}"
        );
        let json = serde_json::to_value(&report).unwrap();
        assert!(json["floor_sweep"].is_null(), "{json}");
    }
}
