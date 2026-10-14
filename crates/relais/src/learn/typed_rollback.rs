//! The rollback rule for a promoted typed research recipe
//! (docs/plans/2026-10-12-task-types.md, Phase 3; ADR-0006).
//!
//! A recipe that routes a type below the implementation tier is kept only
//! while its live runs earn it. Over its live runs of one type, counting
//! only runs that ended with a verdict (an infrastructure stop is unknown
//! and counted neither way), it is rolled back, through the same
//! [`Amendment::for_rollback`] `relais recipe rollback` uses, when
//!
//! - the Wilson 95% lower bound of acceptance without escalation is below
//!   [`MIN_LOWER_BOUND`], or
//! - its cost per accepted task (every attempt of the research run,
//!   escalation included) is at or above the type's Sonnet median cost
//!   of an accepted run, from the drawn arm once it holds
//!   [`DRAWN_ARM_MIN`] runs, else from the ledger's historical Sonnet
//!   runs of the type.
//!
//! It is checked at [`FIRST_CHECK`] live runs and then every
//! [`CHECK_EVERY`], each time over the runs up to that checkpoint. Once
//! rolled back the recipe's effective revision is no longer research, so
//! the rule has nothing left to say about it: it speaks once.

use crate::contract::TaskType;
use crate::ids::RunId;
use crate::ledger::{Ledger, LedgerError, LiveRun};
use crate::policy::{select_highest_enabled_revision, RecipeSpec, RepoPolicy};
use crate::route::is_typed_research_change;
use crate::router::stats::{median, wilson, Z95};

/// Fewer live runs than this are no evidence of anything.
pub const MIN_RUNS: usize = 10;
/// The first checkpoint, in live runs.
pub const FIRST_CHECK: usize = 20;
/// Checkpoints after the first, every this many live runs.
pub const CHECK_EVERY: usize = 10;
/// The Wilson lower bound of acceptance without escalation below which a
/// typed research recipe is rolled back.
pub const MIN_LOWER_BOUND: f64 = 0.50;
/// Runs the drawn Sonnet arm needs before its median replaces history.
pub const DRAWN_ARM_MIN: usize = 5;

/// A live run that ended with a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observed {
    /// Accepted without escalation.
    pub accepted: bool,
    /// Everything the run spent, in micro-dollars.
    pub cost_micros: i64,
}

/// Why a recipe is rolled back.
#[derive(Debug, Clone, PartialEq)]
pub enum Reason {
    LowAcceptance {
        accepted: usize,
        runs: usize,
        lower: f64,
    },
    CostNotBelowSonnet {
        per_accepted_micros: i64,
        sonnet_median_micros: i64,
        runs: usize,
    },
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LowAcceptance {
                accepted,
                runs,
                lower,
            } => write!(
                f,
                "{accepted} of {runs} live runs were accepted without escalation; the Wilson 95% \
                 lower bound {lower:.2} is below {MIN_LOWER_BOUND:.2}"
            ),
            Self::CostNotBelowSonnet {
                per_accepted_micros,
                sonnet_median_micros,
                runs,
            } => write!(
                f,
                "over {runs} live runs it cost {per_accepted_micros} micro-USD per accepted task, \
                 not below the Sonnet median {sonnet_median_micros}"
            ),
        }
    }
}

/// The type's Sonnet median cost of an accepted run: the drawn arm's when
/// it holds [`DRAWN_ARM_MIN`] runs with a verdict, else the history's.
/// `None` when that arm accepted nothing.
pub fn sonnet_median(drawn: &[Observed], historical: &[Observed]) -> Option<i64> {
    let arm = if drawn.len() >= DRAWN_ARM_MIN {
        drawn
    } else {
        historical
    };
    let mut costs: Vec<u64> = arm
        .iter()
        .filter(|run| run.accepted)
        .map(|run| run.cost_micros.max(0) as u64)
        .collect();
    median(&mut costs).map(|micros| micros as i64)
}

/// The rule over one window of runs (oldest first) that ends at a checkpoint.
fn judge_window(window: &[Observed], sonnet_median_micros: Option<i64>) -> Option<Reason> {
    if window.len() < MIN_RUNS {
        return None;
    }
    let accepted = window.iter().filter(|run| run.accepted).count();
    let (lower, _) = wilson(accepted as u64, window.len() as u64, Z95);
    if lower < MIN_LOWER_BOUND {
        return Some(Reason::LowAcceptance {
            accepted,
            runs: window.len(),
            lower,
        });
    }
    let median = sonnet_median_micros?;
    let spent: i64 = window.iter().map(|run| run.cost_micros).sum();
    // spent / accepted >= median, without dividing; `accepted` is above 0
    // here, since a lower bound of at least 0.5 needs an acceptance.
    (i128::from(spent) >= i128::from(median) * accepted as i128).then_some(
        Reason::CostNotBelowSonnet {
            per_accepted_micros: spent / accepted as i64,
            sonnet_median_micros: median,
            runs: window.len(),
        },
    )
}

/// The rule over a recipe's live runs for one type, oldest first, that
/// ended with a verdict: the first reached checkpoint (20, 30, 40, ...)
/// whose window fails it.
pub fn verdict(runs: &[Observed], sonnet_median_micros: Option<i64>) -> Option<Reason> {
    (FIRST_CHECK..=runs.len())
        .step_by(CHECK_EVERY)
        .find_map(|checkpoint| judge_window(&runs[..checkpoint], sonnet_median_micros))
}

/// A promoted typed research recipe the rule rolls back.
#[derive(Debug, Clone, PartialEq)]
pub struct Rollback {
    pub name: String,
    pub task_type: TaskType,
    pub reason: Reason,
}

/// The recipes of `repo` whose effective revision is a typed research
/// change, one per name.
fn promoted(repo: &RepoPolicy) -> Vec<&RecipeSpec> {
    let mut names: Vec<&str> = Vec::new();
    for recipe in &repo.recipes {
        if !names.contains(&recipe.name.as_str()) {
            names.push(&recipe.name);
        }
    }
    names
        .into_iter()
        .filter_map(|name| select_highest_enabled_revision(&repo.recipes, |r| r.name == name))
        .filter(|spec| is_typed_research_change(spec))
        .collect()
}

/// How a run ended: `Some(accepted without escalation)`, or `None` when
/// the ledger has no verdict for it (an infrastructure stop, in flight).
pub type Verdict<'a> = &'a dyn Fn(&RunId) -> Result<Option<bool>, LedgerError>;

/// Every promoted typed research recipe in `repo` that the rule rolls
/// back, from the live runs in `ledger`. `ended` says how a run ended.
pub fn due_rollbacks(
    ledger: &Ledger,
    repo: &RepoPolicy,
    ended: Verdict<'_>,
) -> Result<Vec<Rollback>, LedgerError> {
    let specs = promoted(repo);
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let live = ledger.live_runs_with_first_intent("")?;
    let observe = |run: &RunId| -> Result<Option<Observed>, LedgerError> {
        let Some(accepted) = ended(run)? else {
            return Ok(None);
        };
        Ok(Some(Observed {
            accepted,
            cost_micros: ledger.run_cost(run)?.to_micros(),
        }))
    };
    let mut due = Vec::new();
    for spec in specs {
        let recipe_id = spec.recipe_id();
        let since = ledger
            .recipe_promotions(&recipe_id)?
            .last()
            .map(|promotion| promotion.promoted_at.clone())
            .unwrap_or_default();
        for task_type in &spec.task_type {
            let mut research = Vec::new();
            let mut drawn = Vec::new();
            let mut historical = Vec::new();
            for LiveRun {
                run,
                created_at,
                intent,
            } in &live
            {
                let Some(task) = ledger.task_of_run(run)? else {
                    continue;
                };
                if ledger.routing_type(&task, None)? != Some(*task_type) {
                    continue;
                }
                let tier = intent.get("tier").and_then(|tier| tier.as_str());
                let by_recipe = intent.get("recipe_id").and_then(|id| id.as_str());
                if tier == Some("research") && by_recipe == Some(recipe_id.as_str()) {
                    if *created_at >= since {
                        research.extend(observe(run)?);
                    }
                } else if tier == Some("implementation") {
                    let observed = observe(run)?;
                    historical.extend(observed);
                    if intent["task_type_draw"]["drawn"] == true {
                        drawn.extend(observed);
                    }
                }
            }
            if let Some(reason) = verdict(&research, sonnet_median(&drawn, &historical)) {
                due.push(Rollback {
                    name: spec.name.clone(),
                    task_type: *task_type,
                    reason,
                });
                break;
            }
        }
    }
    Ok(due)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{DispatchId, TaskId};
    use crate::ledger::UsageEvent;
    use crate::money::{CostCompleteness, CostKind, MicroUsd};

    fn runs(accepted: usize, failed: usize, cost: i64) -> Vec<Observed> {
        let one = |accepted| Observed {
            accepted,
            cost_micros: cost,
        };
        (0..accepted)
            .map(|_| one(true))
            .chain((0..failed).map(|_| one(false)))
            .collect()
    }

    #[test]
    fn the_wilson_bound_rolls_back_at_twenty_runs_and_not_before() {
        assert_eq!(MIN_LOWER_BOUND, 0.5);
        // 13 of 20: lower bound ~0.43.
        assert!(matches!(
            verdict(&runs(13, 7, 1), None),
            Some(Reason::LowAcceptance {
                accepted: 13,
                runs: 20,
                ..
            })
        ));
        // 15 of 20: lower bound ~0.53.
        assert_eq!(verdict(&runs(15, 5, 1), None), None);
        // Nothing is checked before 20 live runs, however bad.
        assert_eq!(verdict(&runs(0, 19, 1), None), None);
        assert!(verdict(&runs(0, 20, 1), None).is_some());
    }

    #[test]
    fn it_is_checked_at_twenty_and_then_every_ten() {
        // A good first twenty and a bad stretch after it: the 25-run
        // total is not a checkpoint, the 30-run one is.
        let mut history = runs(20, 0, 1);
        history.extend(runs(0, 5, 1));
        assert_eq!(verdict(&history, None), None);
        history.extend(runs(0, 5, 1));
        assert_eq!(history.len(), 30);
        assert!(matches!(
            verdict(&history, None),
            Some(Reason::LowAcceptance {
                accepted: 20,
                runs: 30,
                ..
            })
        ));
    }

    #[test]
    fn cost_per_accepted_at_or_above_the_sonnet_median_rolls_back() {
        // 20 accepted at 100 each: 100 per accepted.
        let good = runs(20, 0, 100);
        assert!(matches!(
            verdict(&good, Some(100)),
            Some(Reason::CostNotBelowSonnet {
                per_accepted_micros: 100,
                ..
            })
        ));
        assert_eq!(verdict(&good, Some(101)), None);
        // No Sonnet median, no cost comparison.
        assert_eq!(verdict(&good, None), None);
        // A failed run's spend counts against the accepted ones: 18
        // accepted and 2 failed at 100 each cost 111 per accepted.
        let mixed = runs(18, 2, 100);
        assert!(verdict(&mixed, Some(111)).is_some());
        assert_eq!(verdict(&mixed, Some(112)), None);
    }

    #[test]
    fn the_drawn_arm_sets_the_median_once_it_has_five_runs() {
        let arm = |n, cost| runs(n, 0, cost);
        // Four drawn runs: history decides.
        assert_eq!(sonnet_median(&arm(4, 900), &arm(10, 300)), Some(300));
        // Five: the drawn arm decides.
        assert_eq!(sonnet_median(&arm(5, 900), &arm(10, 300)), Some(900));
        // An arm that accepted nothing has no median.
        assert_eq!(sonnet_median(&runs(0, 6, 5), &arm(10, 300)), None);
    }

    const POLICY: &str = r#"schema_version = 1
[models.research]
id = "haiku"
[models.implementation]
id = "sonnet"
[[recipes]]
name = "change-ui"
revision = 1
kind = "change"
scope_within = ["**"]
task_type = ["ui"]
tier = "implementation"
[[recipes]]
name = "change-infra"
revision = 1
kind = "change"
scope_within = ["**"]
task_type = ["infra"]
tier = "implementation"
[[recipes]]
name = "change-ui"
revision = 2
kind = "change"
scope_within = ["**"]
task_type = ["ui"]
tier = "research"
[[recipes]]
name = "change-infra"
revision = 2
kind = "change"
scope_within = ["**"]
task_type = ["infra"]
tier = "research"
"#;

    struct World {
        ledger: Ledger,
        repo: RepoPolicy,
        count: usize,
        _dir: crate::test_support::TempDir,
    }

    /// How a fixture run ended, by the prefix of its id.
    fn ended(run: &RunId) -> Result<Option<bool>, LedgerError> {
        let id = run.as_str();
        Ok(if id.starts_with("ok") {
            Some(true)
        } else if id.starts_with("bad") {
            Some(false)
        } else {
            None
        })
    }

    impl World {
        fn new() -> Self {
            let dir = crate::test_support::short_temp_dir("typed-rollback");
            Self {
                ledger: Ledger::open(&dir.join("ledger.sqlite")).expect("ledger"),
                repo: RepoPolicy::from_toml_str(POLICY).expect("policy parses"),
                count: 0,
                _dir: dir,
            }
        }

        fn recipe_id(&self, index: usize) -> String {
            self.repo.recipes[index].recipe_id()
        }

        /// A live run of `task_type`, named by how it ended (`ok`, `bad`,
        /// `infra`), first dispatched at `tier` under `recipe_id`.
        fn run(
            &mut self,
            outcome: &str,
            task_type: TaskType,
            tier: &str,
            recipe_id: &str,
            drawn: bool,
            cost: i64,
        ) {
            self.count += 1;
            let name = format!("{outcome}-{}", self.count);
            let run = RunId::from_stored(&name);
            let task = TaskId::from_stored(format!("task-{name}"));
            self.ledger
                .insert_typed_run(&run, "/r", None, &task, "rk", Some(task_type))
                .expect("run");
            self.ledger
                .record_dispatch_intent(
                    &DispatchId::from_stored(format!("d-{name}")),
                    &run,
                    None,
                    &serde_json::json!({
                        "tier": tier,
                        "recipe_id": recipe_id,
                        "task_type_draw": drawn.then(|| serde_json::json!({ "drawn": true })),
                    }),
                    0,
                    crate::route::RoutedBy::DeterministicRecipe,
                )
                .expect("dispatch");
            self.ledger
                .record_usage(&UsageEvent {
                    event_id: format!("u-{name}"),
                    run_id: run,
                    attempt_id: None,
                    parent_event_id: None,
                    model: Some("m".into()),
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    cost: Some(MicroUsd::from_micros(cost)),
                    cost_kind: CostKind::ApiSpend,
                    completeness: CostCompleteness::Actual,
                    inclusive: false,
                    at: crate::ledger::now_rfc3339(),
                    phase: None,
                    duration_ms: None,
                    requested_model: None,
                    requested_effort: None,
                    harness: None,
                })
                .expect("usage");
        }

        /// `ok` accepted, `bad` failed and `infra` stopped live research
        /// runs of `task_type`, each costing `cost`.
        fn research(&mut self, recipe: usize, task_type: TaskType, mix: [usize; 3], cost: i64) {
            let id = self.recipe_id(recipe);
            for (outcome, n) in ["ok", "bad", "infra"].into_iter().zip(mix) {
                for _ in 0..n {
                    self.run(outcome, task_type, "research", &id, false, cost);
                }
            }
        }

        fn due(&self) -> Vec<Rollback> {
            due_rollbacks(&self.ledger, &self.repo, &ended).expect("reads")
        }
    }

    #[test]
    fn infrastructure_stops_are_unknown_and_never_count() {
        let mut w = World::new();
        // 15 of 20 is fine; thirty stops beside them change nothing.
        w.research(2, TaskType::Ui, [15, 5, 30], 1);
        assert_eq!(w.due(), Vec::new());
        // Ten accepted and nine failed is 19 runs with a verdict, short of
        // the first checkpoint, whatever the stops add up to.
        let mut w = World::new();
        w.research(2, TaskType::Ui, [10, 9, 40], 1);
        assert_eq!(w.due(), Vec::new());
        // One more failure reaches 20: 10 of 20 is below the bound.
        w.research(2, TaskType::Ui, [0, 1, 0], 1);
        let due = w.due();
        assert_eq!(due.len(), 1);
        assert!(matches!(
            due[0].reason,
            Reason::LowAcceptance { runs: 20, .. }
        ));
    }

    #[test]
    fn rolling_back_one_of_two_promoted_types_leaves_the_other() {
        let mut w = World::new();
        w.research(2, TaskType::Ui, [11, 9, 0], 1);
        w.research(3, TaskType::Infra, [19, 1, 0], 1);
        let due = w.due();
        assert_eq!(
            due.iter()
                .map(|rb| (rb.name.as_str(), rb.task_type))
                .collect::<Vec<_>>(),
            vec![("change-ui", TaskType::Ui)]
        );
        // Applying it through the existing rollback path restores
        // implementation for ui and leaves infra on research.
        let amendment =
            crate::learn::promote::Amendment::for_rollback(&w.repo, "change-ui").expect("rolls");
        let after = amendment.resulting_policy(&w.repo);
        let effective = |name: &str| {
            select_highest_enabled_revision(&after.recipes, |r| r.name == name)
                .map(|spec| spec.tier)
        };
        assert_eq!(
            effective("change-ui"),
            Some(crate::policy::Tier::Implementation)
        );
        assert_eq!(
            effective("change-infra"),
            Some(crate::policy::Tier::Research)
        );
        // Said once: the rolled-back recipe is no longer a research one.
        assert_eq!(promoted(&after).len(), 1);
    }

    #[test]
    fn cost_is_read_against_the_drawn_arm_then_history() {
        let mut w = World::new();
        // 20 accepted live research runs at 100 each.
        w.research(2, TaskType::Ui, [20, 0, 0], 100);
        assert_eq!(w.due(), Vec::new(), "no Sonnet run, no median");
        // Historical Sonnet runs of the type cost 100 each: not below it.
        let rev1 = w.recipe_id(0);
        for _ in 0..3 {
            w.run("ok", TaskType::Ui, "implementation", &rev1, false, 100);
        }
        let due = w.due();
        assert_eq!(due.len(), 1);
        assert!(matches!(
            due[0].reason,
            Reason::CostNotBelowSonnet {
                per_accepted_micros: 100,
                sonnet_median_micros: 100,
                ..
            }
        ));
        // Five drawn runs at 1000 take over from history.
        for _ in 0..5 {
            w.run("ok", TaskType::Ui, "implementation", &rev1, true, 1000);
        }
        assert_eq!(w.due(), Vec::new());
        // Another type's Sonnet cost is not this type's.
        let mut w = World::new();
        w.research(2, TaskType::Ui, [20, 0, 0], 100);
        w.run("ok", TaskType::Infra, "implementation", &rev1, false, 1);
        assert_eq!(w.due(), Vec::new());
    }
}
