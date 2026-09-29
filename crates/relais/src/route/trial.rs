//! Live trial assignment (SPEC §28): which arm a task runs under.
//!
//! [`assign_trial`] is a pure function of its inputs. It reads no clock,
//! no ledger and no file: the caller hands it the machine's envelope, the
//! task, the UTC day, what today's trials have already used, and the
//! candidates it has ALREADY admitted. Admission is not decided here —
//! a candidate reaches this module only after `validate_candidate` and a
//! trust grant have accepted it, so nothing in this file can widen what
//! a trial may run.
//!
//! The draw is `SplitMix64` seeded by a stable hash of (seed, task id,
//! UTC day), uniform over {control} ∪ candidates. Its probability is
//! therefore `1/(1+k)` by construction, and the same inputs always give
//! the same answer — which is what lets the ledger's recorded seed and
//! probability reproduce, and later weight, an assignment.

use sha2::{Digest, Sha256};

use crate::contract::Kind;
use crate::policy::TrialEnvelope;
use crate::rng::SplitMix64;

/// What today's trials have already used, as the ledger counts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DailyUsage {
    /// Trials created since 00:00 UTC.
    pub trials: u32,
    /// Their recorded cost; an unknown cost counted as the full
    /// per-trial ceiling, never zero.
    pub spend_micros: i64,
}

/// One admitted candidate, as an arm: the recipe that would cover this
/// task under the candidate's policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateArm {
    pub recipe_policy_id: String,
}

pub struct TrialInputs<'a> {
    pub envelope: &'a TrialEnvelope,
    pub task_id: &'a str,
    pub kind: Kind,
    /// The UTC day, `YYYY-MM-DD`: part of the draw's key, so a task
    /// re-planned tomorrow is a fresh draw and one re-planned today is
    /// the same one.
    pub utc_day: &'a str,
    pub usage: DailyUsage,
    /// The recipe the incumbent policy covers this task with.
    pub control_recipe_id: &'a str,
    /// Admitted candidates, in the order the envelope lists them. Each is
    /// a DISTINCT arm: the caller has already dropped any that cover the
    /// task with the incumbent's own recipe or with another candidate's,
    /// so `arm_index - 1` indexes this slice.
    pub candidates: &'a [CandidateArm],
}

/// Why a task draws no arm. Typed, so the text a person reads and the
/// test that pins it cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotEligible {
    Disabled,
    Unseeded,
    KindNotEligible,
    DailyCountExhausted,
    DailySpendExhausted,
    NoAdmissibleCandidate,
}

impl std::fmt::Display for NotEligible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Disabled => "disabled",
            Self::Unseeded => "enabled without a seed",
            Self::KindNotEligible => "task kind is not in eligible_kinds",
            Self::DailyCountExhausted => "daily trial count reached",
            Self::DailySpendExhausted => "daily trial spend reached",
            Self::NoAdmissibleCandidate => "no admitted candidate",
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrialAssignment {
    NotEligible {
        reason: NotEligible,
    },
    /// Drawn into the trial, on the incumbent. `arms` lists every arm's
    /// recipe id, control first.
    Control {
        probability: f64,
        arms: Vec<String>,
    },
    /// Drawn onto candidate `arm_index - 1` of the admitted list.
    Assigned {
        arm_index: u32,
        recipe_policy_id: String,
        probability: f64,
        arms: Vec<String>,
    },
}

/// Decide whether this task joins a trial and, if so, on which arm.
///
/// Reasons are checked in a fixed order, cheapest fact first, so the one
/// printed is the first that holds.
pub fn assign_trial(inputs: &TrialInputs<'_>) -> TrialAssignment {
    let envelope = inputs.envelope;
    if !envelope.enabled {
        return not_eligible(NotEligible::Disabled);
    }
    let Some(seed) = envelope.seed else {
        return not_eligible(NotEligible::Unseeded);
    };
    if !envelope.eligible_kinds.contains(&inputs.kind) {
        return not_eligible(NotEligible::KindNotEligible);
    }
    if inputs.usage.trials >= envelope.max_daily_trials {
        return not_eligible(NotEligible::DailyCountExhausted);
    }
    if inputs.usage.spend_micros >= envelope.max_trial_cost_micros {
        return not_eligible(NotEligible::DailySpendExhausted);
    }
    if inputs.candidates.is_empty() {
        return not_eligible(NotEligible::NoAdmissibleCandidate);
    }

    let arms: Vec<String> = std::iter::once(inputs.control_recipe_id.to_string())
        .chain(inputs.candidates.iter().map(|c| c.recipe_policy_id.clone()))
        .collect();
    let probability = 1.0 / arms.len() as f64;
    let drawn = SplitMix64::new(draw_key(seed, inputs.task_id, inputs.utc_day))
        .below(arms.len() as u64) as usize;
    match drawn {
        0 => TrialAssignment::Control { probability, arms },
        index => TrialAssignment::Assigned {
            arm_index: index as u32,
            recipe_policy_id: arms[index].clone(),
            probability,
            arms,
        },
    }
}

fn not_eligible(reason: NotEligible) -> TrialAssignment {
    TrialAssignment::NotEligible { reason }
}

/// A stable 64-bit key for (seed, task id, UTC day). SHA-256, not
/// `DefaultHasher`, whose output std does not promise across releases:
/// a recorded assignment must be reproducible by a later build.
fn draw_key(seed: u64, task_id: &str, utc_day: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"relais-trial-draw-v1\0");
    hasher.update(seed.to_be_bytes());
    hasher.update(b"\0");
    hasher.update(task_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(utc_day.as_bytes());
    let digest = hasher.finalize();
    let mut key = [0u8; 8];
    key.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> TrialEnvelope {
        TrialEnvelope {
            enabled: true,
            seed: Some(7),
            eligible_kinds: vec![Kind::Change],
            max_daily_trials: 5,
            max_trial_cost_micros: 1_000_000,
            candidates: Vec::new(),
        }
    }

    fn arms(ids: &[&str]) -> Vec<CandidateArm> {
        ids.iter()
            .map(|id| CandidateArm {
                recipe_policy_id: (*id).to_string(),
            })
            .collect()
    }

    fn assign(
        envelope: &TrialEnvelope,
        task: &str,
        kind: Kind,
        usage: DailyUsage,
        candidates: &[CandidateArm],
    ) -> TrialAssignment {
        assign_trial(&TrialInputs {
            envelope,
            task_id: task,
            kind,
            utc_day: "2026-09-29",
            usage,
            control_recipe_id: "control",
            candidates,
        })
    }

    fn reason(assignment: TrialAssignment) -> NotEligible {
        match assignment {
            TrialAssignment::NotEligible { reason } => reason,
            other => panic!("expected NotEligible, got {other:?}"),
        }
    }

    #[test]
    fn a_disabled_envelope_is_never_eligible() {
        let off = TrialEnvelope {
            enabled: false,
            ..envelope()
        };
        let got = assign(
            &off,
            "t",
            Kind::Change,
            DailyUsage::default(),
            &arms(&["a"]),
        );
        assert_eq!(reason(got), NotEligible::Disabled);
        // The default envelope is the same thing.
        let default = TrialEnvelope::default();
        let got = assign(
            &default,
            "t",
            Kind::Change,
            DailyUsage::default(),
            &arms(&["a"]),
        );
        assert_eq!(reason(got), NotEligible::Disabled);
    }

    #[test]
    fn enabled_without_a_seed_is_never_drawn() {
        let unseeded = TrialEnvelope {
            seed: None,
            ..envelope()
        };
        let got = assign(
            &unseeded,
            "t",
            Kind::Change,
            DailyUsage::default(),
            &arms(&["a"]),
        );
        assert_eq!(reason(got), NotEligible::Unseeded);
    }

    #[test]
    fn a_kind_outside_eligible_kinds_is_not_eligible_and_empty_means_none() {
        let got = assign(
            &envelope(),
            "t",
            Kind::Inspect,
            DailyUsage::default(),
            &arms(&["a"]),
        );
        assert_eq!(reason(got), NotEligible::KindNotEligible);
        let none = TrialEnvelope {
            eligible_kinds: Vec::new(),
            ..envelope()
        };
        let got = assign(
            &none,
            "t",
            Kind::Change,
            DailyUsage::default(),
            &arms(&["a"]),
        );
        assert_eq!(reason(got), NotEligible::KindNotEligible);
    }

    #[test]
    fn the_daily_count_cap_stops_at_the_cap_and_zero_means_none() {
        let usage = DailyUsage {
            trials: 5,
            spend_micros: 0,
        };
        let got = assign(&envelope(), "t", Kind::Change, usage, &arms(&["a"]));
        assert_eq!(reason(got), NotEligible::DailyCountExhausted);
        let zero = TrialEnvelope {
            max_daily_trials: 0,
            ..envelope()
        };
        let got = assign(
            &zero,
            "t",
            Kind::Change,
            DailyUsage::default(),
            &arms(&["a"]),
        );
        assert_eq!(reason(got), NotEligible::DailyCountExhausted);
        let below = DailyUsage {
            trials: 4,
            spend_micros: 0,
        };
        assert!(!matches!(
            assign(&envelope(), "t", Kind::Change, below, &arms(&["a"])),
            TrialAssignment::NotEligible { .. }
        ));
    }

    #[test]
    fn the_daily_spend_cap_stops_at_the_cap_and_zero_means_none() {
        let usage = DailyUsage {
            trials: 0,
            spend_micros: 1_000_000,
        };
        let got = assign(&envelope(), "t", Kind::Change, usage, &arms(&["a"]));
        assert_eq!(reason(got), NotEligible::DailySpendExhausted);
        let zero = TrialEnvelope {
            max_trial_cost_micros: 0,
            ..envelope()
        };
        let got = assign(
            &zero,
            "t",
            Kind::Change,
            DailyUsage::default(),
            &arms(&["a"]),
        );
        assert_eq!(reason(got), NotEligible::DailySpendExhausted);
    }

    #[test]
    fn no_admitted_candidate_is_no_arm() {
        let got = assign(&envelope(), "t", Kind::Change, DailyUsage::default(), &[]);
        assert_eq!(reason(got), NotEligible::NoAdmissibleCandidate);
    }

    #[test]
    fn the_same_inputs_give_the_same_answer_and_the_key_moves_with_day_and_seed() {
        let candidates = arms(&["a", "b"]);
        let first = assign(
            &envelope(),
            "task-1",
            Kind::Change,
            DailyUsage::default(),
            &candidates,
        );
        let second = assign(
            &envelope(),
            "task-1",
            Kind::Change,
            DailyUsage::default(),
            &candidates,
        );
        assert_eq!(first, second);
        assert_ne!(
            draw_key(7, "task-1", "2026-09-29"),
            draw_key(7, "task-1", "2026-09-30")
        );
        assert_ne!(
            draw_key(7, "task-1", "2026-09-29"),
            draw_key(8, "task-1", "2026-09-29")
        );
    }

    /// Every arm is drawn with frequency 1/(1+k), within ±3 points, over
    /// 10,000 synthetic task ids, for one, two and three candidates.
    ///
    /// FALSIFY: the draw was biased toward control
    /// (`below(arms + 1) / 2` in place of `below(arms)`), and this test
    /// failed on the frequency bound: `k=1 arm 0: frequency 0.6637 vs
    /// expected 0.5`. Then the draw was restored.
    #[test]
    fn each_arm_is_drawn_with_frequency_one_over_one_plus_k() {
        for k in 1..=3usize {
            let ids: Vec<String> = (0..k).map(|i| format!("cand-{i}")).collect();
            let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            let candidates = arms(&id_refs);
            let mut counts = vec![0usize; k + 1];
            for n in 0..10_000 {
                let task = format!("task-{n:05}");
                let got = assign(
                    &envelope(),
                    &task,
                    Kind::Change,
                    DailyUsage::default(),
                    &candidates,
                );
                let (index, probability) = match got {
                    TrialAssignment::Control { probability, arms } => {
                        assert_eq!(arms.len(), k + 1);
                        (0, probability)
                    }
                    TrialAssignment::Assigned {
                        arm_index,
                        probability,
                        recipe_policy_id,
                        arms,
                    } => {
                        assert_eq!(arms[arm_index as usize], recipe_policy_id);
                        (arm_index as usize, probability)
                    }
                    TrialAssignment::NotEligible { reason } => panic!("{reason}"),
                };
                assert_eq!(probability, 1.0 / (k as f64 + 1.0));
                counts[index] += 1;
            }
            let expected = 1.0 / (k as f64 + 1.0);
            for (arm, count) in counts.iter().enumerate() {
                let frequency = *count as f64 / 10_000.0;
                assert!(
                    (frequency - expected).abs() <= 0.03,
                    "k={k} arm {arm}: frequency {frequency} vs expected {expected}"
                );
            }
        }
    }
}
