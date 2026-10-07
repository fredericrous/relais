//! R3 (plan, Packages): the router's measures against hand labels, and the
//! gates routing must clear before it can be switched on. The labels file
//! format is in docs/router-protocol.md, "R3 labels". Pure: the caller
//! reads the file and hands in its bytes.

use serde::{Deserialize, Serialize};

use super::outcome::TaskOutcome;
use super::stats::{clopper_pearson_upper, wilson, Z95};

/// The lower Wilson bound the tier accuracy and the outcome agreement must
/// reach.
pub const LOWER_BOUND_GATE: f64 = 0.75;

/// The highest the missed-failure rate's exact 95% upper bound may be.
pub const MISSED_FAILURE_GATE: f64 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    NewTask,
    Continuation,
    Correction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelTier {
    Research,
    Implementation,
    Escalation,
}

/// What the person labelled. A field left `null` is not measured.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanLabel {
    pub tier: Option<LabelTier>,
    pub relation: Option<Relation>,
    /// Was the task's work actually right?
    pub outcome_correct: Option<bool>,
}

/// What the router decided for the same item.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouterLabel {
    pub tier: Option<LabelTier>,
    pub relation: Option<Relation>,
    pub outcome: Option<TaskOutcome>,
}

/// One line of the labels file.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub item: String,
    pub human: HumanLabel,
    pub router: RouterLabel,
}

/// A line of the labels file that is not a label.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelError {
    /// 1-based.
    pub line: usize,
    pub detail: String,
}

impl std::fmt::Display for LabelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.detail)
    }
}

impl std::error::Error for LabelError {}

/// Every non-blank line as a [`Label`]; item ids must be unique.
pub fn parse_labels(text: &str) -> Result<Vec<Label>, LabelError> {
    let mut labels: Vec<Label> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let label: Label = serde_json::from_str(line).map_err(|e| LabelError {
            line: index + 1,
            detail: e.to_string(),
        })?;
        if labels.iter().any(|seen| seen.item == label.item) {
            return Err(LabelError {
                line: index + 1,
                detail: format!("item `{}` appears twice", label.item),
            });
        }
        labels.push(label);
    }
    Ok(labels)
}

/// One measured rate with its bounds, and its gate when it has one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Measure {
    pub successes: u64,
    pub n: u64,
    pub rate: Option<f64>,
    pub lower: f64,
    pub upper: f64,
    /// The gate in words, `None` for a measure reported without one.
    pub gate: Option<String>,
    pub passed: Option<bool>,
}

fn proportion(successes: u64, n: u64) -> Measure {
    let (lower, upper) = wilson(successes, n, Z95);
    Measure {
        successes,
        n,
        rate: (n > 0).then(|| successes as f64 / n as f64),
        lower,
        upper,
        gate: None,
        passed: None,
    }
}

/// The R3 measures and the verdict.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Evaluation {
    pub schema: u32,
    /// `r3-` and the first 16 hex characters of SHA-256 of the labels
    /// file: what `--record --id` and the plugin's `r3_consent` name.
    pub id: String,
    pub items: usize,
    /// Router tier = human tier, over items with both.
    pub tier_accuracy: Measure,
    /// Router relation `continuation`, over items the person labelled a
    /// continuation. Reported, no gate.
    pub continuation_detection: Measure,
    /// Router outcome agrees with the person's verdict, over items with a
    /// verdict and a strong router outcome (`unknown` excluded): completed
    /// ⇔ correct, corrected ⇔ wrong.
    pub outcome_agreement: Measure,
    /// Router called "completed", over the items the person labelled
    /// wrong. Gated on the exact 95% upper bound.
    pub missed_failure: Measure,
    pub passed: bool,
}

/// The content id of a labels file.
pub fn labels_id(bytes: &[u8]) -> String {
    let hash = crate::ids::sha256_hex(bytes);
    format!("r3-{}", &hash[..16])
}

/// Measure `labels` (from a file whose bytes hash to `id`).
pub fn evaluate(id: String, labels: &[Label]) -> Evaluation {
    let count = |pairs: &mut dyn Iterator<Item = bool>| {
        pairs.fold((0u64, 0u64), |(hit, n), ok| (hit + u64::from(ok), n + 1))
    };

    let (hit, n) = count(
        &mut labels
            .iter()
            .filter_map(|l| Some(l.router.tier? == l.human.tier?)),
    );
    let mut tier_accuracy = proportion(hit, n);
    tier_accuracy.gate = Some(format!("lower bound >= {LOWER_BOUND_GATE}"));
    tier_accuracy.passed = Some(n > 0 && tier_accuracy.lower >= LOWER_BOUND_GATE);

    let (hit, n) = count(
        &mut labels
            .iter()
            .filter(|l| l.human.relation == Some(Relation::Continuation))
            .map(|l| l.router.relation == Some(Relation::Continuation)),
    );
    let continuation_detection = proportion(hit, n);

    let (hit, n) = count(&mut labels.iter().filter_map(|l| {
        let correct = l.human.outcome_correct?;
        let outcome = l.router.outcome.filter(|o| o.is_strong())?;
        Some(outcome.is_completed() == correct)
    }));
    let mut outcome_agreement = proportion(hit, n);
    outcome_agreement.gate = Some(format!("lower bound >= {LOWER_BOUND_GATE}"));
    outcome_agreement.passed = Some(n > 0 && outcome_agreement.lower >= LOWER_BOUND_GATE);

    let (missed, n) = count(
        &mut labels
            .iter()
            .filter(|l| l.human.outcome_correct == Some(false))
            .map(|l| l.router.outcome.is_some_and(TaskOutcome::is_completed)),
    );
    let mut missed_failure = proportion(missed, n);
    missed_failure.upper = clopper_pearson_upper(missed, n, 0.05);
    missed_failure.gate = Some(format!(
        "exact 95% upper bound <= {MISSED_FAILURE_GATE} (0 misses need >= 36 wrong tasks)"
    ));
    missed_failure.passed = Some(n > 0 && missed_failure.upper <= MISSED_FAILURE_GATE);

    let passed = [&tier_accuracy, &outcome_agreement, &missed_failure]
        .iter()
        .all(|measure| measure.passed == Some(true));
    Evaluation {
        schema: 1,
        id,
        items: labels.len(),
        tier_accuracy,
        continuation_detection,
        outcome_agreement,
        missed_failure,
        passed,
    }
}

fn measure_line(name: &str, measure: &Measure) -> String {
    let rate = measure
        .rate
        .map_or_else(|| "n/a".to_string(), |rate| format!("{rate:.3}"));
    let verdict = match measure.passed {
        Some(true) => " — pass",
        Some(false) => " — FAIL",
        None => "",
    };
    let gate = measure
        .gate
        .as_ref()
        .map_or_else(String::new, |gate| format!("; gate: {gate}"));
    format!(
        "{name}: {}/{} = {rate} [{:.3}, {:.3}]{gate}{verdict}\n",
        measure.successes, measure.n, measure.lower, measure.upper
    )
}

impl Evaluation {
    /// The verdict for a person: one line per measure, then the verdict
    /// and the id to record.
    pub fn render(&self) -> String {
        let mut out = format!("r3: {} item(s)\n", self.items);
        out.push_str(&measure_line("tier accuracy", &self.tier_accuracy));
        out.push_str(&measure_line(
            "continuation detection",
            &self.continuation_detection,
        ));
        out.push_str(&measure_line("outcome agreement", &self.outcome_agreement));
        out.push_str(&measure_line(
            "missed failures (router said completed, person said wrong)",
            &self.missed_failure,
        ));
        out.push_str(&format!(
            "verdict: {} (id {})\n",
            if self.passed { "passed" } else { "failed" },
            self.id
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(
        item: usize,
        human_tier: LabelTier,
        router_tier: LabelTier,
        correct: bool,
        outcome: TaskOutcome,
    ) -> Label {
        Label {
            item: format!("i{item}"),
            human: HumanLabel {
                tier: Some(human_tier),
                relation: Some(Relation::NewTask),
                outcome_correct: Some(correct),
            },
            router: RouterLabel {
                tier: Some(router_tier),
                relation: Some(Relation::NewTask),
                outcome: Some(outcome),
            },
        }
    }

    /// `agree` of 120 right items and tiers, plus `wrong` items the person
    /// labelled wrong that the router correctly called `corrected`.
    fn set(agree: usize, wrong: usize) -> Vec<Label> {
        let mut labels = Vec::new();
        for i in 0..120 {
            let ok = i < agree;
            labels.push(label(
                i,
                LabelTier::Implementation,
                if ok {
                    LabelTier::Implementation
                } else {
                    LabelTier::Research
                },
                true,
                if ok {
                    TaskOutcome::CompletedVerified
                } else {
                    TaskOutcome::Corrected
                },
            ));
        }
        for i in 0..wrong {
            labels.push(label(
                1000 + i,
                LabelTier::Escalation,
                LabelTier::Escalation,
                false,
                TaskOutcome::Corrected,
            ));
        }
        labels
    }

    #[test]
    fn the_missed_failure_gate_fails_at_35_wrong_tasks_and_passes_at_36() {
        let at_35 = evaluate("r3-x".into(), &set(120, 35));
        assert_eq!(at_35.missed_failure.n, 35);
        assert_eq!(at_35.missed_failure.successes, 0);
        assert_eq!(at_35.missed_failure.passed, Some(false));
        assert!(!at_35.passed);
        let at_36 = evaluate("r3-x".into(), &set(120, 36));
        assert_eq!(at_36.missed_failure.passed, Some(true));
        assert!(at_36.passed, "{}", at_36.render());
    }

    #[test]
    fn one_miss_among_the_wrong_tasks_fails_the_gate() {
        // 1 of 40: the exact upper bound is about 0.13.
        let mut labels = set(120, 40);
        labels.last_mut().unwrap().router.outcome = Some(TaskOutcome::CompletedAccepted);
        let evaluation = evaluate("r3-x".into(), &labels);
        assert_eq!(evaluation.missed_failure.successes, 1);
        assert_eq!(evaluation.missed_failure.passed, Some(false));
    }

    #[test]
    fn agreement_and_tier_accuracy_gate_between_99_and_100_of_120() {
        // The wrong tasks are left out so only the 120 count.
        let pass = evaluate("r3-x".into(), &set(100, 0));
        assert_eq!(pass.outcome_agreement.n, 120);
        assert_eq!(pass.outcome_agreement.passed, Some(true));
        assert_eq!(pass.tier_accuracy.passed, Some(true));
        let fail = evaluate("r3-x".into(), &set(99, 0));
        assert_eq!(fail.outcome_agreement.passed, Some(false));
        assert_eq!(fail.tier_accuracy.passed, Some(false));
        assert!(!fail.passed);
    }

    #[test]
    fn unknown_router_outcomes_are_not_agreement_evidence() {
        let mut labels = set(120, 0);
        labels[0].router.outcome = Some(TaskOutcome::Unknown);
        assert_eq!(evaluate("r3-x".into(), &labels).outcome_agreement.n, 119);
    }

    #[test]
    fn continuation_detection_is_measured_over_human_continuations() {
        let mut labels = set(120, 0);
        labels[0].human.relation = Some(Relation::Continuation);
        labels[0].router.relation = Some(Relation::Continuation);
        labels[1].human.relation = Some(Relation::Continuation);
        let evaluation = evaluate("r3-x".into(), &labels);
        assert_eq!(evaluation.continuation_detection.successes, 1);
        assert_eq!(evaluation.continuation_detection.n, 2);
        assert_eq!(evaluation.continuation_detection.passed, None);
    }

    #[test]
    fn the_labels_file_is_strict_and_its_id_is_its_content() {
        let line = r#"{"item":"a","human":{"tier":"research","relation":"new_task","outcome_correct":true},"router":{"tier":"research","relation":"new_task","outcome":"completed_verified"}}"#;
        assert_eq!(parse_labels(&format!("{line}\n\n")).unwrap().len(), 1);
        let error = parse_labels(&format!("{line}\n{line}\n")).unwrap_err();
        assert_eq!(error.line, 2);
        let extra = line.replace(r#""item":"a","#, r#""item":"a","note":1,"#);
        assert_eq!(parse_labels(&extra).unwrap_err().line, 1);
        assert_eq!(labels_id(b"x"), labels_id(b"x"));
        assert_ne!(labels_id(b"x"), labels_id(b"y"));
        assert!(labels_id(b"x").starts_with("r3-"));
        assert_eq!(labels_id(b"x").len(), 19);
    }
}
