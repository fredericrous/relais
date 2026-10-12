//! What the decision label rule has collected so far, as `relais report`
//! prints it (orchestrator-decides plan, 4c): yes and no counts by label
//! source, the projected date of the training floor, and how far the
//! orchestrator's reported reverts and the ones relais detects in git agree.
//!
//! The arithmetic is pure and takes already-read values; [`decision_labels`]
//! is the one function that touches the ledger.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::ids::TaskId;
use crate::learn::decision_label::{
    label_decisions, DecisionLabel, DecisionLabelSource, LabelledDecision,
    DECISION_LABEL_POLICY_VERSION,
};
use crate::learn::revert;
use crate::ledger::{Ledger, LedgerError, RevertedReport};
use crate::outcome::SOURCE_ORCHESTRATOR;

/// Yes and no rows the test split must hold, each (plan 4c).
pub const FLOOR_PER_CLASS: usize = 30;
/// The share of outcome-grade labels the test split holds (plan 4c: splits
/// are 60/20/20 by `raised_at`).
pub const TEST_SPLIT_PERCENT: usize = 20;
/// Days of labelled rows, by `raised_at`, the arrival rate is read over.
pub const RATE_DAYS: i64 = 30;
/// How soon after the undo the orchestrator must have reported it for
/// coverage to count it.
pub const COVERAGE_DAYS: i64 = 7;

/// Yes and no labels from one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SourceCount {
    pub source: &'static str,
    pub yes: usize,
    pub no: usize,
}

/// A count of distinct tasks out of a larger count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Fraction {
    pub numerator: usize,
    pub denominator: usize,
}

impl std::fmt::Display for Fraction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} of {}", self.numerator, self.denominator)
    }
}

/// When the test split reaches its floor, projected from the labelled rows
/// of the last [`RATE_DAYS`] days.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FloorProjection {
    /// Yes (and no) rows the outcome-grade labels must hold overall for a
    /// 20% test split to hold [`FLOOR_PER_CLASS`] of each.
    pub needed_per_class: usize,
    /// Outcome-grade yes and no labels so far (`answer` labels never reach
    /// the test split).
    pub yes: usize,
    pub no: usize,
    /// Of those, raised in the last [`RATE_DAYS`] days.
    pub recent_yes: usize,
    pub recent_no: usize,
    /// `min(recent_yes, recent_no)` per week: the scarcer class binds.
    pub per_week: f64,
    /// `None` when the floor is reached or nothing arrived to project from.
    pub weeks_to_floor: Option<f64>,
    /// The projected date (`YYYY-MM-DD`), when there is one.
    pub projected_date: Option<String>,
    pub reached: bool,
}

/// Everything `relais report` says about decision labels.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionLabels {
    pub label_policy_version: u32,
    pub by_source: Vec<SourceCount>,
    /// Answered decisions the rule leaves out.
    pub excluded: usize,
    pub floor: FloorProjection,
    /// Tasks with both an orchestrator-reported and a detected revert, out
    /// of tasks with either.
    pub agreement: Fraction,
    /// Detected reverts the orchestrator also reported within
    /// [`COVERAGE_DAYS`] days of the undo, out of detected reverts (tasks).
    pub coverage: Fraction,
    /// `person-via-orchestrator` answers recorded without the plugin's
    /// proof: zero unless something went around the decide tool.
    pub unproved_proxied: usize,
}

impl DecisionLabels {
    /// No labelled decision yet, projected as of `now` (taken, not read:
    /// the clock is the caller's).
    pub fn empty(now: DateTime<Utc>) -> Self {
        Self {
            label_policy_version: DECISION_LABEL_POLICY_VERSION,
            by_source: count_by_source(&[]).0,
            excluded: 0,
            floor: project_floor(&[], now),
            agreement: Fraction::default(),
            coverage: Fraction::default(),
            unproved_proxied: 0,
        }
    }
}

/// Yes and no counts per label source, in [`DecisionLabelSource::ALL`]
/// order, and the number of excluded decisions.
pub fn count_by_source(rows: &[LabelledDecision]) -> (Vec<SourceCount>, usize) {
    let mut counts: Vec<SourceCount> = DecisionLabelSource::ALL
        .iter()
        .map(|source| SourceCount {
            source: source.as_str(),
            yes: 0,
            no: 0,
        })
        .collect();
    let mut excluded = 0;
    for row in rows {
        let (source, is_yes) = match row.label {
            DecisionLabel::Yes(source) => (source, true),
            DecisionLabel::No(source) => (source, false),
            DecisionLabel::Excluded(_) => {
                excluded += 1;
                continue;
            }
        };
        if let Some(count) = counts
            .iter_mut()
            .find(|count| count.source == source.as_str())
        {
            if is_yes {
                count.yes += 1;
            } else {
                count.no += 1;
            }
        }
    }
    (counts, excluded)
}

/// Project the date the test split reaches its floor. Only outcome-grade
/// labels count (an `answer` label is never in the test split); the arrival
/// rate is the labelled rows by `raised_at` over the last [`RATE_DAYS`]
/// days, never by when the label was written, and the binding class is the
/// scarcer one: `min(yes, no)`.
pub fn project_floor(rows: &[LabelledDecision], now: DateTime<Utc>) -> FloorProjection {
    let needed_per_class = FLOOR_PER_CLASS * 100 / TEST_SPLIT_PERCENT;
    let cutoff = (now - Duration::days(RATE_DAYS)).to_rfc3339();
    let (mut yes, mut no, mut recent_yes, mut recent_no) = (0, 0, 0, 0);
    for row in rows {
        let (source, is_yes) = match row.label {
            DecisionLabel::Yes(source) => (source, true),
            DecisionLabel::No(source) => (source, false),
            DecisionLabel::Excluded(_) => continue,
        };
        if !source.rests_on_an_outcome() {
            continue;
        }
        let recent = row.raised_at >= cutoff;
        match (is_yes, recent) {
            (true, true) => {
                yes += 1;
                recent_yes += 1;
            }
            (true, false) => yes += 1,
            (false, true) => {
                no += 1;
                recent_no += 1;
            }
            (false, false) => no += 1,
        }
    }
    let per_week = recent_yes.min(recent_no) as f64 / (RATE_DAYS as f64 / 7.0);
    let missing = needed_per_class.saturating_sub(yes.min(no));
    let reached = missing == 0;
    let weeks_to_floor = (!reached && per_week > 0.0).then(|| missing as f64 / per_week);
    let projected_date = weeks_to_floor.map(|weeks| {
        (now + Duration::days((weeks * 7.0).ceil() as i64))
            .format("%Y-%m-%d")
            .to_string()
    });
    FloorProjection {
        needed_per_class,
        yes,
        no,
        recent_yes,
        recent_no,
        per_week,
        weeks_to_floor,
        projected_date,
        reached,
    }
}

fn corrupt(what: &str, stamp: &str, e: chrono::ParseError) -> LedgerError {
    LedgerError::Corrupt {
        what: what.to_string(),
        detail: format!("`{stamp}` is not an RFC3339 timestamp: {e}"),
    }
}

fn instant(what: &str, stamp: &str) -> Result<DateTime<Utc>, LedgerError> {
    DateTime::parse_from_rfc3339(stamp)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|e| corrupt(what, stamp, e))
}

/// Agreement and coverage between the orchestrator's reported reverts and
/// the ones relais detected, both over distinct tasks. A `cli` or
/// source-less row is in neither.
///
/// - agreement: tasks with both an orchestrator and a detected revert, over
///   tasks with either;
/// - coverage: detected-revert tasks the orchestrator also reported within
///   [`COVERAGE_DAYS`] days of the undo, over detected-revert tasks. The
///   undo time is the commit's, kept on the detected row (`undo_at`), and
///   the row's own time for one stored without it.
pub fn revert_agreement(reports: &[RevertedReport]) -> Result<(Fraction, Fraction), LedgerError> {
    let mut reported: BTreeMap<&TaskId, Vec<DateTime<Utc>>> = BTreeMap::new();
    let mut detected: BTreeMap<&TaskId, Vec<DateTime<Utc>>> = BTreeMap::new();
    for report in reports {
        match report.source.as_deref() {
            Some(SOURCE_ORCHESTRATOR) => reported.entry(&report.task).or_default().push(instant(
                "the time of a reported revert",
                &report.reported_at,
            )?),
            Some(revert::SOURCE) => {
                let (what, stamp) = match &report.undone_at {
                    Some(undone_at) => ("the time of an undo commit", undone_at),
                    None => ("the time of a detected revert", &report.reported_at),
                };
                detected
                    .entry(&report.task)
                    .or_default()
                    .push(instant(what, stamp)?);
            }
            Some(_) | None => {}
        }
    }
    let either: BTreeSet<&TaskId> = reported.keys().chain(detected.keys()).copied().collect();
    let both = detected
        .keys()
        .filter(|task| reported.contains_key(**task))
        .count();
    let window = Duration::days(COVERAGE_DAYS);
    let covered = detected
        .iter()
        .filter(|(task, undone)| {
            reported.get(**task).is_some_and(|reports| {
                undone
                    .iter()
                    .any(|undo| reports.iter().any(|at| (*at - *undo).abs() <= window))
            })
        })
        .count();
    Ok((
        Fraction {
            numerator: both,
            denominator: either.len(),
        },
        Fraction {
            numerator: covered,
            denominator: detected.len(),
        },
    ))
}

/// Read the ledger and build the section.
pub fn decision_labels(ledger: &Ledger, now: DateTime<Utc>) -> Result<DecisionLabels, LedgerError> {
    let rows = label_decisions(ledger)?;
    let (by_source, excluded) = count_by_source(&rows);
    let (agreement, coverage) = revert_agreement(&ledger.reverted_reports()?)?;
    Ok(DecisionLabels {
        label_policy_version: DECISION_LABEL_POLICY_VERSION,
        by_source,
        excluded,
        floor: project_floor(&rows, now),
        agreement,
        coverage,
        unproved_proxied: ledger
            .person_answer_counts(&now.to_rfc3339())?
            .unproved_proxied,
    })
}

impl DecisionLabels {
    /// The lines `relais report` prints.
    pub fn lines(&self) -> String {
        let mut out = format!(
            "decision labels (policy v{}), yes / no by label source:\n",
            self.label_policy_version
        );
        for count in &self.by_source {
            out.push_str(&format!(
                "  {:<13} {} yes, {} no\n",
                count.source, count.yes, count.no
            ));
        }
        out.push_str(&format!("  not labelled  {}\n", self.excluded));
        let f = &self.floor;
        out.push_str(&format!(
            "  test-split floor: {} of each class needed overall for {FLOOR_PER_CLASS} in a \
             {TEST_SPLIT_PERCENT}% split; outcome-grade labels so far {} yes, {} no; last \
             {RATE_DAYS} days {} yes, {} no ({:.1}/week of the scarcer class); ",
            f.needed_per_class, f.yes, f.no, f.recent_yes, f.recent_no, f.per_week
        ));
        match (&f.projected_date, f.reached) {
            (_, true) => out.push_str("the floor is reached\n"),
            (Some(date), false) => out.push_str(&format!("projected to be reached about {date}\n")),
            (None, false) => out.push_str("no date can be projected\n"),
        }
        out.push_str(&format!(
            "  reverts, orchestrator against git: agreement {} tasks, coverage {} detected \
             reverts reported within {COVERAGE_DAYS} days\n",
            self.agreement, self.coverage
        ));
        if self.unproved_proxied > 0 {
            out.push_str(&format!(
                "  {} person-via-orchestrator answer(s) recorded without the plugin's proof \
                 (recorded_via=cli): not counted as the person's\n",
                self.unproved_proxied
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::RunId;
    use DecisionLabelSource as S;

    fn row(label: DecisionLabel, raised_at: &str) -> LabelledDecision {
        LabelledDecision {
            run: RunId::from_stored("run-x"),
            task: TaskId::from_stored("task-x"),
            raised_at: raised_at.to_string(),
            label,
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-10-12T12:00:00+00:00".parse().expect("time")
    }

    #[test]
    fn counts_by_label_source_match_the_fixture() {
        let rows = vec![
            row(DecisionLabel::Yes(S::Outcome), "2026-10-01T00:00:00+00:00"),
            row(DecisionLabel::Yes(S::Outcome), "2026-10-01T00:00:00+00:00"),
            row(DecisionLabel::No(S::Rewritten), "2026-10-01T00:00:00+00:00"),
            row(DecisionLabel::No(S::Detected), "2026-10-01T00:00:00+00:00"),
            row(
                DecisionLabel::No(S::Orchestrator),
                "2026-10-01T00:00:00+00:00",
            ),
            row(DecisionLabel::Yes(S::Answer), "2026-10-01T00:00:00+00:00"),
            row(DecisionLabel::No(S::Answer), "2026-10-01T00:00:00+00:00"),
            row(DecisionLabel::Excluded("x"), "2026-10-01T00:00:00+00:00"),
        ];
        let (counts, excluded) = count_by_source(&rows);
        let flat: Vec<(&str, usize, usize)> =
            counts.iter().map(|c| (c.source, c.yes, c.no)).collect();
        assert_eq!(
            flat,
            vec![
                ("outcome", 2, 0),
                ("detected", 0, 1),
                ("orchestrator", 0, 1),
                ("rewritten", 0, 1),
                ("answer", 1, 1),
            ]
        );
        assert_eq!(excluded, 1);
    }

    /// 2 yes and 6 no raised in the window: the scarcer class arrives at
    /// 2 / (30 / 7) a week and 150 - 4 (the all-time minimum) are missing.
    /// Rows outside the window count toward the totals and not the rate;
    /// `answer` rows count toward neither.
    #[test]
    fn the_floor_projects_from_min_of_yes_and_no_by_raised_at() {
        let mut rows = vec![
            row(DecisionLabel::Yes(S::Outcome), "2026-10-05T00:00:00+00:00"),
            row(DecisionLabel::Yes(S::Outcome), "2026-10-06T00:00:00+00:00"),
            row(DecisionLabel::Yes(S::Outcome), "2026-07-01T00:00:00+00:00"),
            row(DecisionLabel::Yes(S::Outcome), "2026-07-02T00:00:00+00:00"),
            row(DecisionLabel::Yes(S::Answer), "2026-10-07T00:00:00+00:00"),
        ];
        for day in 1..=6 {
            rows.push(row(
                DecisionLabel::No(S::Rewritten),
                &format!("2026-10-0{day}T00:00:00+00:00"),
            ));
        }
        let projection = project_floor(&rows, now());
        assert_eq!((projection.yes, projection.no), (4, 6));
        assert_eq!((projection.recent_yes, projection.recent_no), (2, 6));
        assert!((projection.per_week - 2.0 * 7.0 / 30.0).abs() < 1e-9);
        let weeks = projection.weeks_to_floor.expect("a projection");
        assert!((weeks - 146.0 / (2.0 * 7.0 / 30.0)).abs() < 1e-6, "{weeks}");
        assert!(projection.projected_date.is_some());
        assert!(!projection.reached);
    }

    #[test]
    fn no_arrivals_means_no_date_and_enough_rows_means_reached() {
        let idle = project_floor(
            &[row(
                DecisionLabel::No(S::Rewritten),
                "2026-07-01T00:00:00+00:00",
            )],
            now(),
        );
        assert_eq!(idle.weeks_to_floor, None);
        assert_eq!(idle.projected_date, None);
        let rows: Vec<LabelledDecision> = (0..150)
            .flat_map(|_| {
                [
                    row(DecisionLabel::Yes(S::Outcome), "2026-10-01T00:00:00+00:00"),
                    row(DecisionLabel::No(S::Detected), "2026-10-01T00:00:00+00:00"),
                ]
            })
            .collect();
        let full = project_floor(&rows, now());
        assert!(full.reached);
        assert_eq!(full.weeks_to_floor, None);
    }

    fn reverted(
        task: &str,
        source: Option<&str>,
        at: &str,
        undone: Option<&str>,
    ) -> RevertedReport {
        RevertedReport {
            task: TaskId::from_stored(task),
            source: source.map(str::to_string),
            reported_at: at.to_string(),
            undone_at: undone.map(str::to_string),
        }
    }

    /// Task a is reverted by both sources, task b by the orchestrator only;
    /// a `cli` row and a source-less row are in neither count.
    #[test]
    fn agreement_is_one_of_two_and_coverage_one_of_one() {
        let reports = vec![
            reverted("a", Some("orchestrator"), "2026-10-02T00:00:00+00:00", None),
            reverted(
                "a",
                Some("detected"),
                "2026-10-09T00:00:00+00:00",
                Some("2026-10-01T00:00:00+00:00"),
            ),
            reverted("b", Some("orchestrator"), "2026-10-03T00:00:00+00:00", None),
            reverted("c", Some("cli"), "2026-10-03T00:00:00+00:00", None),
            reverted("d", None, "2026-10-03T00:00:00+00:00", None),
        ];
        let (agreement, coverage) = revert_agreement(&reports).expect("agreement");
        assert_eq!(agreement.to_string(), "1 of 2");
        assert_eq!(coverage.to_string(), "1 of 1");
    }

    #[test]
    fn a_report_more_than_seven_days_from_the_undo_is_not_coverage() {
        let reports = vec![
            reverted("a", Some("orchestrator"), "2026-10-20T00:00:00+00:00", None),
            reverted(
                "a",
                Some("detected"),
                "2026-10-21T00:00:00+00:00",
                Some("2026-10-01T00:00:00+00:00"),
            ),
        ];
        let (agreement, coverage) = revert_agreement(&reports).expect("agreement");
        assert_eq!(agreement.to_string(), "1 of 1");
        assert_eq!(coverage.to_string(), "0 of 1");
    }

    #[test]
    fn a_detected_row_without_undo_at_uses_its_own_time() {
        let reports = vec![
            reverted("a", Some("detected"), "2026-10-01T00:00:00+00:00", None),
            reverted("a", Some("orchestrator"), "2026-10-02T00:00:00+00:00", None),
        ];
        let (_, coverage) = revert_agreement(&reports).expect("coverage");
        assert_eq!(coverage.to_string(), "1 of 1");
    }

    #[test]
    fn an_unreadable_time_is_corruption_not_a_miss() {
        let reports = vec![reverted("a", Some("detected"), "yesterday", None)];
        assert!(revert_agreement(&reports).is_err());
    }

    fn outcome_from(kind: crate::outcome::OutcomeKind, source: &str) -> crate::outcome::Outcome {
        crate::outcome::Outcome::new(
            kind,
            crate::outcome::OutcomeDetail {
                candidate_sha: None,
                strategy: crate::outcome::Strategy {
                    tier: crate::policy::Tier::Implementation,
                    models: vec![],
                    escalated: false,
                },
                correction_magnitude: None,
                evidence: vec![],
                actor: "orchestrator".into(),
                note: None,
                source: Some(source.into()),
            },
        )
        .expect("outcome")
    }

    /// The ledger path end to end: task a reverted by both sources, task b
    /// by the orchestrator only, task c by a bare `cli` call.
    #[test]
    fn the_ledger_fixture_prints_agreement_one_of_two_and_coverage_one_of_one() {
        let dir = crate::test_support::temp_dir("report-decision-agreement");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let record = |run: &str, task: &str, source: &str| {
            let (run, task) = (RunId::from_stored(run), TaskId::from_stored(task));
            ledger
                .insert_run(&run, "/repo", None, &task, "rk")
                .expect("run");
            ledger
                .record_outcome(
                    &run,
                    &task,
                    &outcome_from(crate::outcome::OutcomeKind::Reverted, source),
                )
                .expect("record");
        };
        record("run-a", "task-a", "orchestrator");
        record("run-b", "task-b", "orchestrator");
        record("run-c", "task-c", "cli");
        let undo_at = Utc::now();
        assert!(ledger
            .record_detected_revert(
                &RunId::from_stored("run-a"),
                &TaskId::from_stored("task-a"),
                &outcome_from(crate::outcome::OutcomeKind::Reverted, "detected"),
                "landing",
                "undo",
                undo_at,
            )
            .expect("detected"));
        let labels = decision_labels(&ledger, Utc::now()).expect("labels");
        assert_eq!(labels.agreement.to_string(), "1 of 2");
        assert_eq!(labels.coverage.to_string(), "1 of 1");
        let text = labels.lines();
        assert!(text.contains("agreement 1 of 2 tasks"), "{text}");
        assert!(text.contains("coverage 1 of 1 detected"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_lines_name_every_figure() {
        let labels = DecisionLabels::empty(now());
        let text = labels.lines();
        assert!(text.contains("decision labels (policy v1)"), "{text}");
        assert!(text.contains("rewritten"), "{text}");
        assert!(text.contains("agreement 0 of 0"), "{text}");
        assert!(text.contains("no date can be projected"), "{text}");
    }
}
