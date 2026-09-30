//! Live trial wiring (SPEC §28): everything around [`assign_trial`] that
//! is not the draw itself.
//!
//! The draw is pure and lives in `route::trial`. This module is where a
//! machine's `[trials]` envelope meets the world: it reads the candidate
//! files, admits each one — parsed, run through [`validate_candidate`]
//! against the repository's CURRENT policy, and required to hold a trust
//! grant for its own authority hash — counts today's trials out of the
//! ledger, asks for the assignment, and records and settles the row.
//!
//! OFF is a value, not a code path that half-runs: with the envelope
//! disabled [`decide`] returns a [`Decision`] whose assignment is `None`
//! before it reads a file or opens a ledger row, and every caller treats
//! `None` as "behave exactly as if this module did not exist".

use std::path::{Path, PathBuf};

use crate::contract::TaskContract;
use crate::ids::{IdError, IdSource, RunId, TaskId, TrialId};
use crate::ledger::{
    Ledger, LedgerError, NewTrial, TrialCost, TrialOutcome, TrialUsage, LIVE_WORKTREE,
};
use crate::money::CostCompleteness;
use crate::policy::{grant_key, MachineSettings, RepoIdentity, RepoPolicy, TrialEnvelope};
use crate::route::trial::{assign_trial, CandidateArm, DailyUsage, TrialAssignment, TrialInputs};
use crate::route::{self, CandidateRecipe};

use super::machine::Terminal;
use super::{utc_day_start, RunOutcome};

/// A candidate the envelope named and admission refused, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    pub path: PathBuf,
    pub why: String,
}

/// A candidate that passed admission: parsed, admissible against the
/// current policy, and granted.
#[derive(Debug, Clone, PartialEq)]
pub struct Admitted {
    pub path: PathBuf,
    policy: CandidateRecipe,
}

impl Admitted {
    pub fn policy(&self) -> &RepoPolicy {
        self.policy.policy()
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Admission {
    pub admitted: Vec<Admitted>,
    pub dropped: Vec<Dropped>,
}

/// Admit each candidate path at run time, never on trust. A relative
/// path is read from the repository root. Any failure drops that one
/// candidate — an ungranted or inadmissible policy never runs.
pub fn admit_candidates(
    root: &Path,
    envelope: &TrialEnvelope,
    repo: &RepoPolicy,
    machine: &MachineSettings,
    identity: &RepoIdentity,
) -> Admission {
    let mut admission = Admission::default();
    for path in &envelope.candidates {
        match admit_one(&root.join(path), repo, machine, identity) {
            Ok(policy) => admission.admitted.push(Admitted {
                path: path.clone(),
                policy,
            }),
            Err(why) => admission.dropped.push(Dropped {
                path: path.clone(),
                why,
            }),
        }
    }
    admission
}

/// The bounds a live-trial candidate is admitted under: the incumbent's
/// own, with the effort catalogs machine.toml states. Admission runs
/// without a harness probe, so what the CLI accepts is unknown here: a
/// candidate whose effort differs from the incumbent's is not admitted to a
/// trial: the conservative answer, never a widened one. (`relais recipe
/// replay` does probe, and judges the same candidate with the real sets.)
fn trial_bounds(repo: &RepoPolicy, machine: &MachineSettings) -> route::TuningBounds {
    let catalogs = crate::catalog::EffortCatalogs::resolve_all(
        &crate::catalog::Fact::Unknown,
        &machine.efforts,
        &machine.routing.max_effort,
        repo.models.values().map(|profile| profile.id.as_str()),
    );
    route::default_tuning_bounds(repo, machine.allowed_models.as_deref(), &catalogs)
}

fn admit_one(
    path: &Path,
    repo: &RepoPolicy,
    machine: &MachineSettings,
    identity: &RepoIdentity,
) -> Result<CandidateRecipe, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read it: {e}"))?;
    let parsed = RepoPolicy::from_toml_str(&text).map_err(|e| format!("does not parse: {e}"))?;
    let bounds = trial_bounds(repo, machine);
    let candidate = route::validate_candidate(repo, &parsed, &bounds)
        .map_err(|rejection| format!("inadmissible: {rejection}"))?;
    let authority_hash = candidate.policy().authority_hash();
    let key = grant_key(&authority_hash, identity);
    if !machine.trust.contains_key(&key) {
        return Err(format!(
            "no trust grant for its authority hash {authority_hash} (grant key {key})"
        ));
    }
    Ok(candidate)
}

/// Everything [`decide`] reads. `ledger` is `None` for `relais plan` on a
/// machine that has no ledger yet: plan writes nothing, so it must not
/// create one just to learn that today's usage is zero.
pub struct Environment<'a> {
    pub root: &'a Path,
    pub repo: &'a RepoPolicy,
    pub machine: &'a MachineSettings,
    pub identity: &'a RepoIdentity,
    pub contract: &'a TaskContract,
    pub task_id: &'a TaskId,
    pub ledger: Option<&'a Ledger>,
}

/// The outcome of asking whether this task joins a trial.
#[derive(Debug)]
pub struct Decision {
    /// `None` exactly when the envelope is off: nothing to print, record
    /// or run differently.
    assignment: Option<TrialAssignment>,
    dropped: Vec<Dropped>,
    arms: Vec<Admitted>,
    control_recipe_id: String,
    seed: u64,
}

impl Decision {
    fn off() -> Self {
        Self {
            assignment: None,
            dropped: Vec::new(),
            arms: Vec::new(),
            control_recipe_id: String::new(),
            seed: 0,
        }
    }

    /// The lines `plan` and `run` print: each dropped candidate and why,
    /// then the assignment. Empty when the envelope is off.
    pub fn lines(&self) -> Vec<String> {
        let Some(assignment) = &self.assignment else {
            return Vec::new();
        };
        let mut lines: Vec<String> = self
            .dropped
            .iter()
            .map(|dropped| {
                format!(
                    "trial: candidate {} dropped: {}",
                    dropped.path.display(),
                    dropped.why
                )
            })
            .collect();
        lines.push(match assignment {
            TrialAssignment::NotEligible { reason } => {
                format!("trial: not eligible ({reason})")
            }
            TrialAssignment::Control { probability, arms } => {
                format!("trial: control (p={probability:.2}, arms={})", arms.len())
            }
            TrialAssignment::Assigned {
                arm_index,
                probability,
                ..
            } => {
                let path = self
                    .candidate_at(*arm_index)
                    .map(|admitted| admitted.path.display().to_string())
                    .unwrap_or_default();
                format!("trial: candidate {path} (p={probability:.2})")
            }
        });
        lines
    }

    /// Whether an arm was drawn, control included: the only case that
    /// earns a `trials` row.
    pub fn draws(&self) -> bool {
        matches!(
            self.assignment,
            Some(TrialAssignment::Control { .. } | TrialAssignment::Assigned { .. })
        )
    }

    fn candidate_at(&self, arm_index: u32) -> Option<&Admitted> {
        let candidate_index = usize::try_from(arm_index).ok()?.checked_sub(1)?;
        self.arms.get(candidate_index)
    }

    /// The policy the run executes under when a candidate was drawn.
    /// `None` for control, not-eligible and off: the incumbent runs.
    pub fn candidate_policy(&self) -> Option<&RepoPolicy> {
        match self.assignment.as_ref()? {
            TrialAssignment::Assigned { arm_index, .. } => {
                self.candidate_at(*arm_index).map(Admitted::policy)
            }
            TrialAssignment::Control { .. } | TrialAssignment::NotEligible { .. } => None,
        }
    }

    /// Write the one `trials` row this assignment earns, at assignment —
    /// control included. `None` when nothing was drawn.
    pub fn record(
        &self,
        ledger: &Ledger,
        ids: &IdSource,
        facts: &TrialFacts<'_>,
        run_id: &RunId,
    ) -> Result<Option<TrialId>, TrialError> {
        let (arm_index, arm_recipe_id, probability, arms) = match self.assignment.as_ref() {
            None | Some(TrialAssignment::NotEligible { .. }) => return Ok(None),
            Some(TrialAssignment::Control { probability, arms }) => {
                (0, self.control_recipe_id.as_str(), *probability, arms)
            }
            Some(TrialAssignment::Assigned {
                arm_index,
                recipe_policy_id,
                probability,
                arms,
            }) => (*arm_index, recipe_policy_id.as_str(), *probability, arms),
        };
        let trial_id = ids.trial_id()?;
        let arms_json =
            serde_json::to_string(arms).map_err(|e| TrialError::Encode(e.to_string()))?;
        ledger.insert_trial(&NewTrial {
            trial_id: &trial_id,
            task_id: facts.task_id,
            // The live run's OWN id, minted before the row is written and
            // handed to `execute`. It is not a replay source: there is no
            // paired replay for a live run, so evaluation estimates live
            // rows unpaired, by the randomized estimator (SPEC §28).
            source_run_id: run_id,
            incumbent_recipe_id: &self.control_recipe_id,
            arm_recipe_id,
            arm_index,
            assignment_probability: probability,
            seed: self.seed,
            base_sha: facts.base_sha,
            contract_hash: facts.contract_hash,
            verification_profile_hash: facts.verification_profile_hash,
            workspace_isolation: LIVE_WORKTREE,
            arms_json: Some(&arms_json),
            arm_run_id: run_id,
        })?;
        Ok(Some(trial_id))
    }
}

/// What a trial row records about the task, known before it runs.
pub struct TrialFacts<'a> {
    pub task_id: &'a TaskId,
    pub base_sha: &'a str,
    pub contract_hash: &'a str,
    pub verification_profile_hash: &'a str,
}

#[derive(Debug)]
pub enum TrialError {
    Ids(IdError),
    Ledger(LedgerError),
    Encode(String),
}

impl std::fmt::Display for TrialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ids(e) => write!(f, "{e}"),
            Self::Ledger(e) => write!(f, "{e}"),
            Self::Encode(e) => write!(f, "cannot encode the arm list: {e}"),
        }
    }
}

impl std::error::Error for TrialError {}

impl From<IdError> for TrialError {
    fn from(e: IdError) -> Self {
        Self::Ids(e)
    }
}

impl From<LedgerError> for TrialError {
    fn from(e: LedgerError) -> Self {
        Self::Ledger(e)
    }
}

/// Today's UTC day start (RFC3339, ledger stamp format) and its date, by
/// the ledger's own clock when there is one.
fn today(ledger: Option<&Ledger>) -> Result<(String, String), LedgerError> {
    let now = ledger
        .map(Ledger::now)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let start = utc_day_start(&now).ok_or_else(|| LedgerError::Corrupt {
        what: "clock".into(),
        detail: format!("{now:?} is not an RFC3339 timestamp"),
    })?;
    let date = start.chars().take(10).collect();
    Ok((start, date))
}

/// What today's trials have used, counted from the ledger.
pub fn usage_today(
    ledger: Option<&Ledger>,
    envelope: &TrialEnvelope,
) -> Result<TrialUsage, LedgerError> {
    let Some(ledger) = ledger else {
        return Ok(TrialUsage {
            trials: 0,
            spend_micros: 0,
        });
    };
    let (start, _date) = today(Some(ledger))?;
    ledger.trial_usage_since(&start, envelope.max_trial_cost_micros)
}

/// Ask whether `env.task_id` joins a trial and on which arm.
pub fn decide(env: &Environment<'_>) -> Result<Decision, LedgerError> {
    let envelope = &env.machine.trials;
    if !envelope.enabled {
        return Ok(Decision::off());
    }
    let usage = usage_today(env.ledger, envelope)?;
    let (_start, utc_day) = today(env.ledger)?;

    let eligible = route::eligible_tiers(env.contract, env.repo, &env.repo.models);
    let control_recipe_id =
        route::covering_recipe_id(env.contract, env.repo, &eligible.tiers).unwrap_or_default();

    let Admission {
        admitted,
        mut dropped,
    } = admit_candidates(env.root, envelope, env.repo, env.machine, env.identity);
    let mut arms: Vec<Admitted> = Vec::new();
    let mut arm_ids: Vec<CandidateArm> = Vec::new();
    for candidate in admitted {
        let policy = candidate.policy();
        let tiers = route::eligible_tiers(env.contract, policy, &policy.models);
        let Some(recipe_policy_id) = route::covering_recipe_id(env.contract, policy, &tiers.tiers)
        else {
            dropped.push(Dropped {
                path: candidate.path,
                why: "no recipe in it covers this task".into(),
            });
            continue;
        };
        if recipe_policy_id == control_recipe_id
            || arm_ids
                .iter()
                .any(|arm| arm.recipe_policy_id == recipe_policy_id)
        {
            dropped.push(Dropped {
                path: candidate.path,
                why: "covers this task with a recipe that is already an arm".into(),
            });
            continue;
        }
        arm_ids.push(CandidateArm { recipe_policy_id });
        arms.push(candidate);
    }

    let assignment = assign_trial(&TrialInputs {
        envelope,
        task_id: env.task_id.as_str(),
        kind: env.contract.kind(),
        utc_day: &utc_day,
        usage: DailyUsage {
            trials: usage.trials,
            spend_micros: usage.spend_micros,
        },
        control_recipe_id: &control_recipe_id,
        candidates: &arm_ids,
    });
    Ok(Decision {
        assignment: Some(assignment),
        dropped,
        arms,
        control_recipe_id,
        // `assign_trial` only draws with a seed; the fallback is never
        // recorded because no row is written without a draw.
        seed: envelope.seed.unwrap_or_default(),
    })
}

/// A trial's outcome from a run's terminal state, and whether it was
/// accepted without escalation. The one mapping `dataset replay` and a
/// live arm share: an infrastructure end is `Errored`, never a failure
/// of the arm.
pub fn trial_outcome_of(terminal: &Terminal) -> (TrialOutcome, bool) {
    match terminal {
        Terminal::Accepted(receipt) => (TrialOutcome::Accepted, receipt.attempts <= 1),
        Terminal::Failed { .. } | Terminal::NeedsReview { .. } | Terminal::NeedsDecision { .. } => {
            (TrialOutcome::Rejected, false)
        }
        Terminal::Blocked { .. }
        | Terminal::BudgetExhausted { .. }
        | Terminal::Interrupted { .. }
        | Terminal::Cancelled { .. } => (TrialOutcome::Errored, false),
    }
}

/// The cost (paired with its completeness) and duration a run settled
/// with, in the form a trial row takes them.
pub fn settled_figures(ledger: &Ledger, run_id: &RunId) -> Result<(TrialCost, i64), LedgerError> {
    let completeness = ledger.run_cost_completeness(run_id)?;
    let cost_value = match completeness {
        CostCompleteness::Unknown => None,
        CostCompleteness::Actual
        | CostCompleteness::Estimated
        | CostCompleteness::IncompleteLowerBound => Some(ledger.run_cost(run_id)?),
    };
    let cost = TrialCost::new(cost_value, completeness).expect(
        "a cost/completeness pair read back from the ledger's own settled values is always \
         a consistent one",
    );
    let duration_ms = ledger
        .run_duration_seconds(run_id)?
        .map(|seconds| (seconds * 1000.0) as i64)
        .unwrap_or(0);
    Ok((cost, duration_ms))
}

/// Settle a live arm once, at the run's terminal state.
pub fn settle(
    ledger: &Ledger,
    trial_id: &TrialId,
    outcome: &RunOutcome,
) -> Result<TrialOutcome, LedgerError> {
    let (trial_outcome, accepted_without_escalation) = trial_outcome_of(&outcome.terminal);
    let (cost, duration_ms) = settled_figures(ledger, &outcome.run_id)?;
    ledger.settle_trial(
        trial_id,
        trial_outcome,
        accepted_without_escalation,
        cost,
        duration_ms,
    )?;
    Ok(trial_outcome)
}

/// Settle a live arm whose run never produced an outcome: an error is
/// `Errored` with unknown cost, so the row is not left in flight for the
/// daily caps to count at full price forever.
pub fn settle_errored(ledger: &Ledger, trial_id: &TrialId) -> Result<(), LedgerError> {
    ledger.settle_trial(
        trial_id,
        TrialOutcome::Errored,
        false,
        TrialCost::UNKNOWN,
        0,
    )
}

/// What `relais doctor` says about the envelope.
#[derive(Debug)]
pub struct Status {
    pub seed_set: bool,
    pub admission: Admission,
    pub usage: TrialUsage,
}

/// The envelope's standing, for doctor: admission is judged against the
/// policy and grants as they are now.
pub fn status(
    root: &Path,
    repo: Option<&RepoPolicy>,
    machine: &MachineSettings,
    identity: &RepoIdentity,
    ledger: Option<&Ledger>,
) -> Result<Status, LedgerError> {
    let envelope = &machine.trials;
    let admission = match repo {
        Some(repo) => admit_candidates(root, envelope, repo, machine, identity),
        None => Admission::default(),
    };
    Ok(Status {
        seed_set: envelope.seed.is_some(),
        admission,
        usage: usage_today(ledger, envelope)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{short_temp_dir, TempDir};

    fn base_policy(review: &str) -> String {
        format!(
            "schema_version = 1\n\
             [models.implementation]\nid = \"sonnet\"\n\
             [[recipes]]\nname = \"src\"\nscope_within = [\"src/**\"]\n\
             tier = \"implementation\"\nrevision = 0\nreview = \"{review}\"\n"
        )
    }

    fn revision_one(review: &str) -> String {
        format!(
            "{}[[recipes]]\nname = \"src\"\nscope_within = [\"src/**\"]\n\
             tier = \"implementation\"\nrevision = 1\nreview = \"{review}\"\n",
            base_policy("required")
        )
    }

    fn contract() -> TaskContract {
        TaskContract::from_json_str(
            r#"{"schema_version":1,"kind":"change","objective":"x","base_ref":"HEAD",
                "write_scope":["src/**"],"acceptance":["done"],
                "verification_profile":"default","review":"off"}"#,
        )
        .expect("contract parses")
    }

    fn identity() -> RepoIdentity {
        RepoIdentity::origin("https://example.test/r.git")
    }

    /// machine.toml with the given `[trials]` body and a grant for each
    /// named policy's authority hash.
    fn machine(trials: &str, granted: &[&RepoPolicy]) -> MachineSettings {
        let mut text = format!("schema_version = 1\n[trials]\n{trials}\n");
        for policy in granted {
            let key = grant_key(&policy.authority_hash(), &identity());
            text.push_str(&format!(
                "[trust.\"{key}\"]\ngranted_at = \"2026-09-18\"\nreviewed_by = \"test\"\n"
            ));
        }
        MachineSettings::from_toml_str(&text).expect("machine parses")
    }

    fn write(dir: &TempDir, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).expect("write");
        path
    }

    fn trials_body(candidates: &[&PathBuf], seed: &str, max_trials: u32) -> String {
        let listed = candidates
            .iter()
            .map(|path| format!("{:?}", path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "enabled = true\n{seed}\neligible_kinds = [\"change\"]\n\
             max_daily_trials = {max_trials}\nmax_trial_cost_micros = 1000000\n\
             candidates = [{listed}]\n"
        )
    }

    /// An admitted candidate needs all three: it parses, it is admissible
    /// against the CURRENT policy, and its own authority hash is granted.
    /// A missing file, an inadmissible policy and an ungranted one are each
    /// dropped with the reason named.
    #[test]
    fn each_candidate_is_dropped_unless_it_parses_is_admissible_and_is_granted() {
        let dir = short_temp_dir("lt-admit");
        let incumbent = RepoPolicy::from_toml_str(&base_policy("required")).expect("incumbent");
        let good_text = revision_one("required");
        let good = RepoPolicy::from_toml_str(&good_text).expect("good");
        let good_path = write(&dir, "good.toml", &good_text);
        let lowered_text = revision_one("off");
        let lowered = RepoPolicy::from_toml_str(&lowered_text).expect("lowered parses");
        let lowered_path = write(&dir, "lowered.toml", &lowered_text);
        let ungranted_text = format!(
            "{good_text}[[recipes]]\nname = \"src\"\nscope_within = [\"src/**\"]\n\
             tier = \"implementation\"\nrevision = 2\nreview = \"required\"\n"
        );
        let ungranted_path = write(&dir, "ungranted.toml", &ungranted_text);
        let missing_path = dir.join("missing.toml");
        let garbage_path = write(&dir, "garbage.toml", "this is = = not toml");

        // Everything but `ungranted` holds a grant, so the lowering one
        // fails on admissibility alone, not on a missing grant.
        let machine = machine(
            &trials_body(
                &[
                    &good_path,
                    &lowered_path,
                    &ungranted_path,
                    &missing_path,
                    &garbage_path,
                ],
                "seed = 1",
                5,
            ),
            &[&good, &lowered],
        );
        let admission = admit_candidates(&dir, &machine.trials, &incumbent, &machine, &identity());
        let admitted: Vec<&PathBuf> = admission.admitted.iter().map(|a| &a.path).collect();
        assert_eq!(admitted, vec![&good_path]);
        let why = |path: &PathBuf| {
            admission
                .dropped
                .iter()
                .find(|dropped| &dropped.path == path)
                .map(|dropped| dropped.why.clone())
                .unwrap_or_else(|| panic!("{} was not dropped", path.display()))
        };
        assert!(
            why(&lowered_path).contains("inadmissible"),
            "{}",
            why(&lowered_path)
        );
        assert!(
            why(&lowered_path).contains("review"),
            "{}",
            why(&lowered_path)
        );
        assert!(
            why(&ungranted_path).contains("no trust grant"),
            "{}",
            why(&ungranted_path)
        );
        assert!(
            why(&missing_path).contains("cannot read"),
            "{}",
            why(&missing_path)
        );
        assert!(
            why(&garbage_path).contains("does not parse"),
            "{}",
            why(&garbage_path)
        );
    }

    #[test]
    fn off_decides_nothing_and_touches_nothing() {
        let dir = short_temp_dir("lt-off");
        let repo = RepoPolicy::from_toml_str(&base_policy("required")).expect("repo");
        let machine = machine("enabled = false\nseed = 1\nmax_daily_trials = 9\n", &[]);
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let task = TaskId::from_stored("task-1");
        let contract = contract();
        let decision = decide(&Environment {
            root: &dir,
            repo: &repo,
            machine: &machine,
            identity: &identity(),
            contract: &contract,
            task_id: &task,
            ledger: Some(&ledger),
        })
        .expect("decides");
        assert!(decision.lines().is_empty());
        assert!(!decision.draws());
        assert!(decision.candidate_policy().is_none());
        let ids = IdSource::new(std::time::SystemTime::now, std::process::id());
        let recorded = decision
            .record(
                &ledger,
                &ids,
                &TrialFacts {
                    task_id: &task,
                    base_sha: "b",
                    contract_hash: "c",
                    verification_profile_hash: "p",
                },
                &ids.run_id().expect("run id"),
            )
            .expect("record");
        assert_eq!(recorded, None);
        assert_eq!(ledger.trial_usage_since("0", 1).expect("usage").trials, 0);
    }

    /// One row per eligible task, control included, counted back out of the
    /// ledger: with `max_daily_trials = 2` the third task is not eligible,
    /// however many processes or restarts came between.
    #[test]
    fn the_daily_count_is_read_from_the_ledger_not_from_memory() {
        let dir = short_temp_dir("lt-cap");
        let repo = RepoPolicy::from_toml_str(&base_policy("required")).expect("repo");
        let cand_text = revision_one("required");
        let cand = RepoPolicy::from_toml_str(&cand_text).expect("cand");
        let cand_path = write(&dir, "cand.toml", &cand_text);
        let machine = machine(&trials_body(&[&cand_path], "seed = 3", 2), &[&cand]);
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let contract = contract();
        let ids = IdSource::new(std::time::SystemTime::now, std::process::id());
        let mut lines = Vec::new();
        for n in 0..3 {
            let task = TaskId::from_stored(format!("task-{n}"));
            let decision = decide(&Environment {
                root: &dir,
                repo: &repo,
                machine: &machine,
                identity: &identity(),
                contract: &contract,
                task_id: &task,
                ledger: Some(&ledger),
            })
            .expect("decides");
            lines.push(decision.lines().join("\n"));
            let recorded = decision
                .record(
                    &ledger,
                    &ids,
                    &TrialFacts {
                        task_id: &task,
                        base_sha: "b",
                        contract_hash: "c",
                        verification_profile_hash: "p",
                    },
                    &ids.run_id().expect("run id"),
                )
                .expect("record");
            assert_eq!(recorded.is_some(), n < 2, "task {n}: {}", lines[n]);
            if let Some(trial_id) = recorded {
                let arms = ledger.trial_arms(&trial_id).expect("arms").expect("a draw");
                assert_eq!(arms.len(), 2, "control and one candidate");
                let row = ledger.trials_by_task(&task).expect("rows").remove(0);
                assert_eq!(row.workspace_isolation, LIVE_WORKTREE);
                assert_eq!(row.seed, 3);
                assert_eq!(row.assignment_probability, 0.5);
                assert_eq!(row.outcome, None, "unsettled until the run ends");
                let cost = TrialCost::new(
                    Some(crate::money::MicroUsd::from_micros(100)),
                    CostCompleteness::Actual,
                )
                .expect("a figure with a completeness");
                ledger
                    .settle_trial(&trial_id, TrialOutcome::Accepted, true, cost, 1)
                    .expect("settle");
            }
        }
        assert!(
            lines[2].contains("not eligible (daily trial count reached)"),
            "{}",
            lines[2]
        );
        let usage = ledger.trial_usage_since("0", 1_000_000).expect("usage");
        assert_eq!(usage.trials, 2);
        assert_eq!(usage.spend_micros, 200, "the recorded figures, summed");
    }

    /// A trial with no recorded cost — in flight, or settled with unknown
    /// usage — counts at the full `max_trial_cost_micros`, never zero, so
    /// one such trial is enough to reach a cap of that size.
    #[test]
    fn a_trial_with_no_recorded_cost_counts_at_the_full_ceiling() {
        let dir = short_temp_dir("lt-unknown");
        let repo = RepoPolicy::from_toml_str(&base_policy("required")).expect("repo");
        let cand_text = revision_one("required");
        let cand = RepoPolicy::from_toml_str(&cand_text).expect("cand");
        let cand_path = write(&dir, "cand.toml", &cand_text);
        let machine = machine(&trials_body(&[&cand_path], "seed = 3", 9), &[&cand]);
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let contract = contract();
        let ids = IdSource::new(std::time::SystemTime::now, std::process::id());
        let mut lines = Vec::new();
        for n in 0..2 {
            let task = TaskId::from_stored(format!("task-{n}"));
            let decision = decide(&Environment {
                root: &dir,
                repo: &repo,
                machine: &machine,
                identity: &identity(),
                contract: &contract,
                task_id: &task,
                ledger: Some(&ledger),
            })
            .expect("decides");
            lines.push(decision.lines().join("\n"));
            decision
                .record(
                    &ledger,
                    &ids,
                    &TrialFacts {
                        task_id: &task,
                        base_sha: "b",
                        contract_hash: "c",
                        verification_profile_hash: "p",
                    },
                    &ids.run_id().expect("run id"),
                )
                .expect("record");
        }
        assert!(lines[0].contains("trial: "), "{}", lines[0]);
        assert!(
            lines[1].contains("not eligible (daily trial spend reached)"),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn enabled_without_a_seed_draws_nothing_and_records_nothing() {
        let dir = short_temp_dir("lt-unseeded");
        let repo = RepoPolicy::from_toml_str(&base_policy("required")).expect("repo");
        let cand_text = revision_one("required");
        let cand = RepoPolicy::from_toml_str(&cand_text).expect("cand");
        let cand_path = write(&dir, "cand.toml", &cand_text);
        let machine = machine(&trials_body(&[&cand_path], "", 5), &[&cand]);
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let task = TaskId::from_stored("task-1");
        let contract = contract();
        let decision = decide(&Environment {
            root: &dir,
            repo: &repo,
            machine: &machine,
            identity: &identity(),
            contract: &contract,
            task_id: &task,
            ledger: Some(&ledger),
        })
        .expect("decides");
        assert!(!decision.draws());
        assert_eq!(
            decision.lines(),
            vec!["trial: not eligible (enabled without a seed)".to_string()]
        );
    }
}
