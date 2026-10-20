//! Trials: controlled comparisons, their settlement, ground truth and the
//! recipe promotions they authorise.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::ids::{RunId, TaskId, TrialId};
use crate::money::{CostCompleteness, MicroUsd};

use super::{parse_trial_row, Ledger, LedgerError, Result};

/// One arm of one controlled comparison (SPEC §17), as the ledger holds
/// it: which task and source run it derives from, the incumbent and arm
/// recipes being compared, the arm's index and the assignment
/// probability it was drawn with, the seed and identity inputs that let
/// it reproduce, how its workspace was isolated, and — once settled —
/// what happened to it.
///
/// Storage and a reader only: nothing in this crate constructs one
/// through an assignment decision. `outcome`, `accepted_without_escalation`,
/// `cost`, `cost_completeness` and `duration_ms` are all `None` until
/// something settles the row; `cost: None` means unknown usage, never a
/// A settled trial's cost together with how complete that figure is, as
/// ONE value so the two cannot disagree.
///
/// They were two arguments, and that made two impossible states
/// representable: a cost of zero marked `Unknown` — so a trial whose
/// usage was never reported read as FREE — and a missing cost marked
/// `Actual`, an actual figure that is not there. The ledger already
/// refuses this for usage rows: migration v2 normalises with
/// `CASE WHEN completeness = '"unknown"' THEN NULL ELSE cost_micros END`.
/// The rule is the same here; it is enforced in the type rather than
/// restated in SQL, so the write path and the read path cannot drift.
///
/// `Unknown` carries no figure by construction. Every other completeness
/// requires one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrialCost {
    cost: Option<MicroUsd>,
    completeness: CostCompleteness,
}

/// Why a cost and its completeness could not be paired.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrialCostError {
    /// `Unknown` with a figure: if the figure is real, say how complete
    /// it is; if it is not, there is no figure.
    UnknownWithAFigure,
    /// A figure's completeness with no figure — `Actual` names an amount
    /// that is absent.
    FigureCompletenessWithoutAFigure(CostCompleteness),
}

impl std::fmt::Display for TrialCostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownWithAFigure => write!(
                f,
                "a trial cost marked `unknown` carries no figure; a figure needs a completeness \
                 that describes it"
            ),
            Self::FigureCompletenessWithoutAFigure(completeness) => write!(
                f,
                "a trial cost marked `{completeness:?}` names a figure, and none was given"
            ),
        }
    }
}

impl std::error::Error for TrialCostError {}

impl TrialCost {
    /// The only way to build one. A trial whose usage was never reported.
    pub const UNKNOWN: Self = Self {
        cost: None,
        completeness: CostCompleteness::Unknown,
    };

    pub fn new(
        cost: Option<MicroUsd>,
        completeness: CostCompleteness,
    ) -> std::result::Result<Self, TrialCostError> {
        match (cost, completeness) {
            (Some(_), CostCompleteness::Unknown) => Err(TrialCostError::UnknownWithAFigure),
            (None, CostCompleteness::Unknown) => Ok(Self::UNKNOWN),
            (Some(cost), completeness) => Ok(Self {
                cost: Some(cost),
                completeness,
            }),
            (None, completeness) => Err(TrialCostError::FigureCompletenessWithoutAFigure(
                completeness,
            )),
        }
    }

    /// The figure, when there is one. `None` is UNKNOWN, never zero.
    pub fn cost(self) -> Option<MicroUsd> {
        self.cost
    }

    pub fn completeness(self) -> CostCompleteness {
        self.completeness
    }
}

/// Hand-written rather than derived: a derived `Deserialize` would read
/// `cost`/`completeness` straight onto the struct's private fields,
/// bypassing [`TrialCost::new`] and reintroducing exactly the
/// impossible pair the private fields exist to rule out — a stored
/// report a caller edited into "0, but unknown" would deserialize
/// without complaint. Routing through `new` keeps that refused on read,
/// not merely on construction.
impl Serialize for TrialCost {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Repr {
            cost: Option<MicroUsd>,
            completeness: CostCompleteness,
        }
        Repr {
            cost: self.cost,
            completeness: self.completeness,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TrialCost {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Repr {
            cost: Option<MicroUsd>,
            completeness: CostCompleteness,
        }
        let repr = Repr::deserialize(deserializer)?;
        TrialCost::new(repr.cost, repr.completeness).map_err(serde::de::Error::custom)
    }
}

/// The `workspace_isolation` a live trial arm records: the arm ran in the
/// repository's own task worktree, where the source run's accepted answer
/// is NOT hidden, and it has no replay source to pair with.
pub const LIVE_WORKTREE: &str = "live_worktree";

/// What the trials created since some instant have used (SPEC §28).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrialUsage {
    pub trials: u32,
    /// Recorded cost, with every trial whose cost is not on record
    /// (unsettled, or settled with unknown usage) counted at the figure
    /// the caller names, never as zero.
    pub spend_micros: i64,
}

/// free trial (SPEC §11).
#[derive(Debug, Clone, PartialEq)]
pub struct TrialRow {
    pub trial_id: TrialId,
    pub task_id: TaskId,
    pub source_run_id: RunId,
    pub incumbent_recipe_id: String,
    pub arm_recipe_id: String,
    pub arm_index: u32,
    pub assignment_probability: f64,
    pub seed: u64,
    pub base_sha: String,
    pub contract_hash: String,
    pub verification_profile_hash: String,
    pub workspace_isolation: String,
    pub outcome: Option<TrialOutcome>,
    pub accepted_without_escalation: Option<bool>,
    /// `None` until the arm is settled. Once settled, the figure and its
    /// completeness travel together and cannot disagree.
    pub cost: Option<TrialCost>,
    pub duration_ms: Option<i64>,
    pub created_at: String,
    /// The run the arm itself executed as: the replay's own run for a
    /// replay, the run itself for a live trial. `None` on a row written
    /// before ledger step v17, whose arm run is not on record.
    pub arm_run_id: Option<RunId>,
    /// SHA-256 of the program design the arm was handed; `None` when it
    /// was handed none (and on every row written before ledger step v29).
    pub design_sha256: Option<String>,
    /// The person's verdict on a review-rejected replay, once judged
    /// (`relais dataset judge-trial`); `None` until then and on every row
    /// written before ledger step v32.
    pub ground_truth: Option<GroundTruth>,
}

/// What a person found when they checked a review-rejected replay against
/// the source work: did the candidate hold up (`Pass`) or not (`Fail`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GroundTruthVerdict {
    Pass,
    Fail,
}

impl GroundTruthVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }

    pub(super) fn parse(stored: &str) -> Option<Self> {
        match stored {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            _ => None,
        }
    }
}

/// A verdict with the evidence the person gave and when it was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroundTruth {
    pub verdict: GroundTruthVerdict,
    pub note: String,
    /// RFC 3339, from the ledger clock.
    pub at: String,
}

/// Why a trial cannot take the verdict it was offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeRefusal {
    UnknownTrial,
    /// Only a review-rejected trial is judged; this one's outcome is
    /// recorded here (`None`: not settled).
    NotRejected(Option<TrialOutcome>),
    EmptyNote,
    /// The trial already carries the other verdict, and a verdict is final.
    Conflict(GroundTruth),
}

/// What offering a verdict did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judged {
    Recorded(GroundTruth),
    /// The trial already carried this verdict; nothing was written.
    AlreadyRecorded(GroundTruth),
}

/// What recording a fresh trial row needs — everything a comparison
/// knows about an arm before it has run (SPEC §17). Borrowed, like
/// [`EvidenceOrigin`], because every caller already owns these as typed
/// values or `&str`s and a trial is written once, at the point of
/// assignment.
#[derive(Debug, Clone, Copy)]
pub struct NewTrial<'a> {
    pub trial_id: &'a TrialId,
    pub task_id: &'a TaskId,
    pub source_run_id: &'a RunId,
    pub incumbent_recipe_id: &'a str,
    pub arm_recipe_id: &'a str,
    pub arm_index: u32,
    pub assignment_probability: f64,
    pub seed: u64,
    pub base_sha: &'a str,
    pub contract_hash: &'a str,
    pub verification_profile_hash: &'a str,
    pub workspace_isolation: &'a str,
    /// The JSON list of every arm's recipe id, when the arm was drawn
    /// among several. `None` for a replay.
    pub arms_json: Option<&'a str>,
    /// The run the arm executed as; see [`TrialRow::arm_run_id`].
    pub arm_run_id: &'a RunId,
    /// See [`TrialRow::design_sha256`].
    pub design_sha256: Option<&'a str>,
}

/// What recording a REPLAY's arm needs. Grouped rather than passed
/// positionally, and without an `#[allow(clippy::too_many_arguments)]`:
/// a prior run's own acceptance criteria, stored in this repository's
/// receipts, read "no new `_ =>` arm over an enum this crate owns, no new
/// bool parameter, no new `#[allow]`" — and the established answer to
/// `too_many_arguments` here is to group, as `DecisionAnswer` and
/// [`NewTrial`] beside it already do.
///
/// The fields a replay does NOT get to choose are absent by construction:
/// `arm_index`, `assignment_probability` and `seed` are set by
/// `insert_replay_trial` itself, because a replay is one deliberate
/// re-run rather than a draw, and a caller that could name them could
/// describe a sampled comparison that never happened.
#[derive(Debug, Clone, Copy)]
pub struct NewReplayTrial<'a> {
    pub trial_id: &'a TrialId,
    pub task_id: &'a TaskId,
    pub source_run_id: &'a RunId,
    pub incumbent_recipe_id: &'a str,
    pub arm_recipe_id: &'a str,
    pub base_sha: &'a str,
    pub contract_hash: &'a str,
    pub verification_profile_hash: &'a str,
    pub workspace_isolation: &'a str,
    /// The replay's own run, known once `execute` has returned.
    pub arm_run_id: &'a RunId,
    /// SHA-256 of the design `dataset replay --design` handed the arm.
    pub design_sha256: Option<&'a str>,
}

/// What recording a recipe promotion's provenance needs.
#[derive(Debug, Clone, Copy)]
pub struct NewRecipePromotion<'a> {
    pub recipe_id: &'a str,
    pub recipe_name: &'a str,
    pub revision: u32,
    pub task_type: &'a str,
    /// The gate that authorised it, by name.
    pub gate: &'a str,
    /// The trials the gate counted, in the order it counted them.
    pub trial_ids: &'a [TrialId],
}

/// A recorded promotion: the trials that authorised a recipe revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipePromotion {
    pub recipe_id: String,
    pub recipe_name: String,
    pub revision: u32,
    pub task_type: String,
    pub gate: String,
    pub trial_ids: Vec<TrialId>,
    pub promoted_at: String,
}

/// Raw columns of one `trials` row, in the order [`parse_trial_row`]
/// expects them.
#[allow(clippy::type_complexity)]
pub(super) type TrialColumns = (
    String,
    String,
    String,
    String,
    String,
    i64,
    f64,
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<i64>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

impl Ledger {
    /// Record one arm of one comparison (SPEC §17).
    ///
    /// What keeps a model out of this is NOT the visibility of this
    /// function — an earlier comment claimed `pub` amounted to a
    /// crate-only boundary, which it never did, and narrowing it to
    /// `pub(crate)` only made both writers dead code while nothing
    /// assigns trials yet. A worker is a separate process running shell
    /// commands; it cannot call a Rust function at any visibility.
    ///
    /// The boundary that does hold: no CLI subcommand or flag accepts a
    /// trial id, an arm index or an assignment probability, and no
    /// `TaskContract` field feeds one. A trial's arm and probability come
    /// from relais's own assignment (not yet written), never from
    /// anything a model can put in a file or a command. When assignment
    /// arrives it must keep that true — and a test over the CLI surface,
    /// not a visibility modifier, is what will say so.
    /// Record `relais dataset replay`'s one deliberate arm (SPEC §24):
    /// nothing was drawn, so this fixes what an ordinary trial's arm index
    /// and assignment probability are for a replay, rather than handing
    /// either to a caller that has no business naming them — the CLI must
    /// not be ABLE to spell out which trials column an arm or a draw
    /// probability lands in (`nothing_a_model_can_reach_names_a_trials_arm`
    /// below), and a plain positional signature here is what keeps that
    /// true after this call exists, not merely before it.
    pub fn insert_replay_trial(&self, replay: &NewReplayTrial<'_>) -> Result<()> {
        self.insert_trial(&NewTrial {
            trial_id: replay.trial_id,
            task_id: replay.task_id,
            source_run_id: replay.source_run_id,
            incumbent_recipe_id: replay.incumbent_recipe_id,
            arm_recipe_id: replay.arm_recipe_id,
            // A replay is one deliberate re-run, not a draw among several
            // arms: there is only ever one, assigned with certainty.
            arm_index: 0,
            assignment_probability: 1.0,
            seed: 0,
            base_sha: replay.base_sha,
            contract_hash: replay.contract_hash,
            verification_profile_hash: replay.verification_profile_hash,
            workspace_isolation: replay.workspace_isolation,
            arms_json: None,
            arm_run_id: replay.arm_run_id,
            design_sha256: replay.design_sha256,
        })
    }

    /// Every arm recipe id a trial was drawn among, control first, or
    /// `None` for a trial that was not a draw (a replay) or no such trial.
    pub fn trial_arms(&self, trial_id: &TrialId) -> Result<Option<Vec<String>>> {
        let stored: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT arms_json FROM trials WHERE trial_id = ?1",
                [trial_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        stored
            .flatten()
            .map(|json| {
                serde_json::from_str(&json).map_err(|e| LedgerError::Corrupt {
                    what: "trials.arms_json".into(),
                    detail: e.to_string(),
                })
            })
            .transpose()
    }

    /// How many trials were created at or after `since` (RFC3339, in the
    /// ledger's own stamp format), and what they cost. The daily caps are
    /// counted HERE, from the rows, never from a process's memory: two
    /// concurrent runs and a restart all see the same number.
    /// `unknown_cost_micros` is what a trial with no recorded figure
    /// counts as: the caller passes the per-trial ceiling, so an
    /// unsettled or unpriced trial can never read as free.
    pub fn trial_usage_since(&self, since: &str, unknown_cost_micros: i64) -> Result<TrialUsage> {
        let (trials, spend_micros): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(COALESCE(cost_micros, ?2)), 0)
               FROM trials WHERE created_at >= ?1",
            params![since, unknown_cost_micros],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(TrialUsage {
            trials: u32::try_from(trials).unwrap_or(u32::MAX),
            spend_micros,
        })
    }

    pub fn insert_trial(&self, trial: &NewTrial<'_>) -> Result<()> {
        let now = self.now();
        self.conn.execute(
            "INSERT INTO trials
                (trial_id, task_id, source_run_id, incumbent_recipe_id,
                 arm_recipe_id, arm_index, assignment_probability, seed,
                 base_sha, contract_hash, verification_profile_hash,
                 workspace_isolation, created_at, arms_json, arm_run_id, design_sha256)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                trial.trial_id.as_str(),
                trial.task_id.as_str(),
                trial.source_run_id.as_str(),
                trial.incumbent_recipe_id,
                trial.arm_recipe_id,
                trial.arm_index,
                trial.assignment_probability,
                trial.seed as i64,
                trial.base_sha,
                trial.contract_hash,
                trial.verification_profile_hash,
                trial.workspace_isolation,
                now,
                trial.arms_json,
                trial.arm_run_id.as_str(),
                trial.design_sha256,
            ],
        )?;
        Ok(())
    }

    /// Settle a trial: what happened to its arm, once it is known. Cost
    /// is `None` for unknown usage, never zero (SPEC §11) — a trial
    /// whose usage was never reported must not read as free.
    /// Settle one arm, once. The cost arrives already paired with its
    /// completeness ([`TrialCost`]), so a zero marked `unknown` — a trial
    /// reading as FREE when its usage was never reported — cannot be
    /// passed in.
    ///
    /// `WHERE trial_id = ?1 AND outcome IS NULL` and the affected-row
    /// count together make this settle-once: an unknown id changes no
    /// row, and a trial already settled changes no row, and both are
    /// errors rather than a silent `Ok(())`. A settlement that vanished
    /// because of a typo, or a first outcome quietly replaced by a
    /// second, are the two ways this row stops being the one record of
    /// what happened to the arm.
    pub fn settle_trial(
        &self,
        trial_id: &TrialId,
        outcome: TrialOutcome,
        accepted_without_escalation: bool,
        cost: TrialCost,
        duration_ms: i64,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE trials SET outcome = ?2, accepted_without_escalation = ?3,
                cost_micros = ?4, cost_completeness = ?5, duration_ms = ?6,
                settled_at = ?7
             WHERE trial_id = ?1 AND outcome IS NULL",
            params![
                trial_id.as_str(),
                outcome.as_str(),
                accepted_without_escalation,
                cost.cost().map(MicroUsd::to_micros),
                serde_json::to_string(&cost.completeness()).expect("completeness serializes"),
                duration_ms,
                self.now(),
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::Corrupt {
                what: "trials.settle".into(),
                detail: format!(
                    "settling trial {} changed {changed} rows, not 1: either no such trial, or \
                     it was already settled and its first outcome must not be overwritten",
                    trial_id.as_str()
                ),
            });
        }
        Ok(())
    }

    /// Record which trials authorised a recipe's promotion. Append-only:
    /// a recipe promoted twice has two rows, and none is ever rewritten.
    pub fn record_recipe_promotion(&self, promotion: &NewRecipePromotion<'_>) -> Result<()> {
        let trial_ids: Vec<&str> = promotion.trial_ids.iter().map(TrialId::as_str).collect();
        self.conn.execute(
            "INSERT INTO recipe_promotions
                (recipe_id, recipe_name, revision, task_type, gate, trial_ids_json, promoted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                promotion.recipe_id,
                promotion.recipe_name,
                promotion.revision,
                promotion.task_type,
                promotion.gate,
                serde_json::to_string(&trial_ids).expect("trial ids serialize"),
                self.now(),
            ],
        )?;
        Ok(())
    }

    /// The promotions recorded for one recipe id, oldest first.
    pub fn recipe_promotions(&self, recipe_id: &str) -> Result<Vec<RecipePromotion>> {
        let mut stmt = self.conn.prepare(
            "SELECT recipe_id, recipe_name, revision, task_type, gate, trial_ids_json, promoted_at
               FROM recipe_promotions WHERE recipe_id = ?1 ORDER BY promotion_id",
        )?;
        let rows = stmt.query_map([recipe_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let rows: Vec<_> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(recipe_id, recipe_name, revision, task_type, gate, trials, promoted_at)| {
                    let trial_ids: Vec<String> =
                        serde_json::from_str(&trials).map_err(|e| LedgerError::Corrupt {
                            what: "recipe_promotions.trial_ids_json".into(),
                            detail: e.to_string(),
                        })?;
                    Ok(RecipePromotion {
                        recipe_id,
                        recipe_name,
                        revision,
                        task_type,
                        gate,
                        trial_ids: trial_ids.into_iter().map(TrialId::from_stored).collect(),
                        promoted_at,
                    })
                },
            )
            .collect()
    }

    const TRIAL_COLUMNS: &'static str = "trial_id, task_id, source_run_id, incumbent_recipe_id,
         arm_recipe_id, arm_index, assignment_probability, seed, base_sha,
         contract_hash, verification_profile_hash, workspace_isolation,
         outcome, accepted_without_escalation, cost_micros, cost_completeness,
         duration_ms, created_at, arm_run_id, design_sha256,
         ground_truth, ground_truth_note, ground_truth_at";

    /// One trial by id, or `None` when no such trial is recorded.
    pub fn trial(&self, trial_id: &TrialId) -> Result<Option<TrialRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM trials WHERE trial_id = ?1",
            Self::TRIAL_COLUMNS
        ))?;
        let rows = stmt.query_map([trial_id.as_str()], Self::trial_columns)?;
        let rows: Vec<TrialColumns> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().next().map(parse_trial_row).transpose()
    }

    /// Offer a ground-truth verdict for a review-rejected trial. Refused,
    /// writing nothing, for an unknown trial, a trial whose outcome is not
    /// `rejected`, an empty note, or a trial already judged the other way;
    /// the same verdict again writes nothing and says so.
    pub fn judge_trial(
        &self,
        trial_id: &TrialId,
        verdict: GroundTruthVerdict,
        note: &str,
    ) -> Result<std::result::Result<Judged, JudgeRefusal>> {
        let Some(trial) = self.trial(trial_id)? else {
            return Ok(Err(JudgeRefusal::UnknownTrial));
        };
        if trial.outcome != Some(TrialOutcome::Rejected) {
            return Ok(Err(JudgeRefusal::NotRejected(trial.outcome)));
        }
        if note.trim().is_empty() {
            return Ok(Err(JudgeRefusal::EmptyNote));
        }
        if let Some(existing) = trial.ground_truth {
            return Ok(if existing.verdict == verdict {
                Ok(Judged::AlreadyRecorded(existing))
            } else {
                Err(JudgeRefusal::Conflict(existing))
            });
        }
        let truth = GroundTruth {
            verdict,
            note: note.trim().to_string(),
            at: self.now(),
        };
        let changed = self.conn.execute(
            "UPDATE trials SET ground_truth = ?2, ground_truth_note = ?3, ground_truth_at = ?4
              WHERE trial_id = ?1 AND ground_truth IS NULL AND outcome = 'rejected'",
            params![trial_id.as_str(), verdict.as_str(), truth.note, truth.at],
        )?;
        if changed != 1 {
            return Err(LedgerError::Corrupt {
                what: "trials.ground_truth".into(),
                detail: format!(
                    "judging trial {} changed {changed} rows, not 1",
                    trial_id.as_str()
                ),
            });
        }
        Ok(Ok(Judged::Recorded(truth)))
    }

    /// Every trial recorded for one task, oldest first — a typed reader:
    /// a row with an unrecognised outcome or cost completeness is a
    /// [`LedgerError::Corrupt`], never silently normalised.
    pub fn trials_by_task(&self, task_id: &TaskId) -> Result<Vec<TrialRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM trials WHERE task_id = ?1 ORDER BY created_at, trial_id",
            Self::TRIAL_COLUMNS
        ))?;
        let rows = stmt.query_map([task_id.as_str()], Self::trial_columns)?;
        let rows: Vec<TrialColumns> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().map(parse_trial_row).collect()
    }

    /// Every trial whose source run is this one, oldest first. See
    /// [`Ledger::trials_by_task`] for the corruption guarantee.
    pub fn trials_by_run(&self, source_run_id: &RunId) -> Result<Vec<TrialRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM trials WHERE source_run_id = ?1 ORDER BY created_at, trial_id",
            Self::TRIAL_COLUMNS
        ))?;
        let rows = stmt.query_map([source_run_id.as_str()], Self::trial_columns)?;
        let rows: Vec<TrialColumns> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().map(parse_trial_row).collect()
    }

    /// Every SETTLED trial whose arm ran under one of the given recipe
    /// ids, oldest first. A candidate is a whole policy, and different
    /// tasks may cover under different recipes within it, so "the trials
    /// for this candidate" means every arm whose `arm_recipe_id` names a
    /// recipe the candidate itself declares — never a single recipe id,
    /// and never an unsettled row still in flight. See
    /// [`Ledger::trials_by_task`] for the corruption guarantee.
    pub fn settled_trials_for_recipes(
        &self,
        recipe_ids: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<TrialRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM trials WHERE outcome IS NOT NULL ORDER BY created_at, trial_id",
            Self::TRIAL_COLUMNS
        ))?;
        let rows = stmt.query_map([], Self::trial_columns)?;
        let rows: Vec<TrialColumns> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(parse_trial_row)
            .filter(|row| match row {
                Ok(row) => recipe_ids.contains(&row.arm_recipe_id),
                Err(_) => true,
            })
            .collect()
    }

    /// Every SETTLED live (`live_worktree`) trial whose recorded arm list
    /// (`arms_json`, parsed as the JSON list it is) names one of the given
    /// recipe ids among its NON-control arms (`arms[1..]`; entry 0 is the
    /// control's own id, which a candidate shares with its base), oldest
    /// first. Selection is on the draw, not on the arm
    /// that ran: a CONTROL row's own `arm_recipe_id` is the incumbent's, yet
    /// it is comparison evidence for a candidate that was among the arms when
    /// it was drawn — and a control row drawn without the candidate never is.
    /// A row whose `arms_json` is not a JSON list of strings is
    /// [`LedgerError::Corrupt`].
    pub fn settled_live_trials_for_arms(
        &self,
        recipe_ids: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<TrialRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {}, arms_json FROM trials
              WHERE outcome IS NOT NULL AND workspace_isolation = ?1 AND arms_json IS NOT NULL
              ORDER BY created_at, trial_id",
            Self::TRIAL_COLUMNS
        ))?;
        let rows = stmt.query_map([LIVE_WORKTREE], |row| {
            Ok((Self::trial_columns(row)?, row.get::<_, String>(23)?))
        })?;
        let rows: Vec<(TrialColumns, String)> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let mut kept = Vec::new();
        for (columns, arms_json) in rows {
            let arms: Vec<String> =
                serde_json::from_str(&arms_json).map_err(|e| LedgerError::Corrupt {
                    what: "trials.arms_json".into(),
                    detail: e.to_string(),
                })?;
            if arms.iter().skip(1).any(|arm| recipe_ids.contains(arm)) {
                kept.push(parse_trial_row(columns)?);
            }
        }
        Ok(kept)
    }

    /// Maps one `trials` row, in [`Self::TRIAL_COLUMNS`]'s order, into
    /// the raw tuple [`parse_trial_row`] expects.
    fn trial_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrialColumns> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
            row.get(10)?,
            row.get(11)?,
            row.get(12)?,
            row.get(13)?,
            row.get(14)?,
            row.get(15)?,
            row.get(16)?,
            row.get(17)?,
            row.get(18)?,
            row.get(19)?,
            row.get(20)?,
            row.get(21)?,
            row.get(22)?,
        ))
    }
}

/// What became of one trial arm's run, once settled (SPEC §17). `NULL`
/// on the row means "not settled yet" — a trial in flight — never an
/// unnamed outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialOutcome {
    /// The arm's candidate was accepted.
    Accepted,
    /// Verification (or a person) concluded the candidate should not be
    /// accepted.
    Rejected,
    /// The arm's run ended on an infrastructure fault — a crash, a
    /// cancellation — rather than a verification verdict, so it is not
    /// comparable evidence for or against the arm.
    Errored,
    /// The replay did not run the arm it was recorded under: its first
    /// attempt dispatched another model than the arm's, or the design it
    /// was handed disagrees with the arm's `requires_design`
    /// (docs/plans/2026-10-12-task-types.md). Never evidence for or
    /// against any arm, and never counted as the arm's failure.
    Invalid,
}

impl TrialOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Errored => "errored",
            Self::Invalid => "invalid",
        }
    }

    pub(super) fn parse(stored: &str) -> Option<Self> {
        match stored {
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "errored" => Some(Self::Errored),
            "invalid" => Some(Self::Invalid),
            _ => None,
        }
    }
}

impl std::fmt::Display for TrialOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{new_trial, run, task, temp_ledger, trial};
    use super::*;

    /// A freshly inserted trial reads back with every field it was
    /// given, unsettled: `outcome`, `accepted_without_escalation`,
    /// `cost`, `cost_completeness` and `duration_ms` all absent, not
    /// zero or a guessed default.
    #[test]
    fn a_fresh_trial_reads_back_unsettled() {
        let (ledger, dir) = temp_ledger();
        let t = trial("trial-1");
        let source_task = task("t1");
        let source_run = run("run-1");
        ledger
            .insert_run(&source_run, "/repo", None, &source_task, "rk")
            .expect("run");
        ledger
            .insert_trial(&new_trial(&t, &source_task, &source_run))
            .expect("insert trial");
        let rows = ledger.trials_by_task(&source_task).expect("trials");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.trial_id, t);
        assert_eq!(row.task_id, source_task);
        assert_eq!(row.source_run_id, source_run);
        assert_eq!(row.incumbent_recipe_id, "recipe-incumbent");
        assert_eq!(row.arm_recipe_id, "recipe-arm");
        assert_eq!(row.arm_index, 1);
        assert_eq!(row.assignment_probability, 0.25);
        assert_eq!(row.seed, 42);
        assert_eq!(row.base_sha, "base-sha");
        assert_eq!(row.contract_hash, "contract-hash");
        assert_eq!(row.verification_profile_hash, "profile-hash");
        assert_eq!(row.workspace_isolation, "worktree");
        assert_eq!(row.outcome, None);
        assert_eq!(row.accepted_without_escalation, None);
        assert_eq!(
            row.cost, None,
            "unreported usage is unknown, never a free trial"
        );
        assert!(row.cost.is_none(), "an unsettled arm has no cost pair");
        assert_eq!(row.duration_ms, None);
        assert_eq!(
            ledger.trials_by_run(&source_run).expect("trials"),
            rows,
            "the same row, read by its source run"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Settling a trial fills in exactly the fields settlement carries,
    /// and a NULL cost stays NULL when usage genuinely was never
    /// reported — it never rounds to zero.
    #[test]
    fn settling_a_trial_records_its_outcome_and_never_defaults_an_unknown_cost() {
        let (ledger, dir) = temp_ledger();
        let t = trial("trial-2");
        let source_task = task("t2");
        let source_run = run("run-2");
        ledger
            .insert_run(&source_run, "/repo", None, &source_task, "rk")
            .expect("run");
        ledger
            .insert_trial(&new_trial(&t, &source_task, &source_run))
            .expect("insert trial");
        ledger
            .settle_trial(&t, TrialOutcome::Accepted, true, TrialCost::UNKNOWN, 4_500)
            .expect("settle");
        let row = ledger
            .trials_by_task(&source_task)
            .expect("trials")
            .remove(0);
        assert_eq!(row.outcome, Some(TrialOutcome::Accepted));
        assert_eq!(row.accepted_without_escalation, Some(true));
        let cost = row.cost.expect("a settled trial has a paired cost");
        assert_eq!(cost.cost(), None, "usage was never reported, not free");
        assert_eq!(cost.completeness(), CostCompleteness::Unknown);
        assert_eq!(row.duration_ms, Some(4_500));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The two combinations a cost and its completeness must never form.
    /// Both were representable while they were separate arguments, and
    /// the first is the one that mattered: a zero marked `unknown` made a
    /// trial whose usage was never reported read as FREE.
    #[test]
    fn a_cost_and_its_completeness_cannot_disagree() {
        assert_eq!(
            TrialCost::new(Some(MicroUsd::from_micros(0)), CostCompleteness::Unknown),
            Err(TrialCostError::UnknownWithAFigure),
            "a figure marked unknown is a figure that reads as free"
        );
        assert_eq!(
            TrialCost::new(None, CostCompleteness::Actual),
            Err(TrialCostError::FigureCompletenessWithoutAFigure(
                CostCompleteness::Actual
            )),
            "an `actual` figure that is not there"
        );
        // And the pairs that are coherent.
        assert_eq!(
            TrialCost::new(None, CostCompleteness::Unknown),
            Ok(TrialCost::UNKNOWN)
        );
        for completeness in [
            CostCompleteness::Actual,
            CostCompleteness::Estimated,
            CostCompleteness::IncompleteLowerBound,
        ] {
            let paired = TrialCost::new(Some(MicroUsd::from_micros(7)), completeness)
                .expect("a figure with a completeness describing it");
            assert_eq!(paired.cost(), Some(MicroUsd::from_micros(7)));
            assert_eq!(paired.completeness(), completeness);
        }
    }

    /// A row stored with the disagreeing pair — by an older binary, or by
    /// hand — is corrupt on the way out, not a trial that reads as free.
    #[test]
    fn a_stored_cost_disagreeing_with_its_completeness_is_corrupt() {
        let (ledger, dir) = temp_ledger();
        let source_task = TaskId::from_stored("task-c");
        let source_run = run("run-c");
        ledger
            .insert_run(&source_run, "/repo", None, &source_task, "rk")
            .expect("run");
        let t = TrialId::from_stored("trial-c");
        ledger
            .insert_trial(&new_trial(&t, &source_task, &source_run))
            .expect("insert trial");
        // Zero micros, marked unknown: what `settle_trial` can no longer
        // be asked to write.
        ledger
            .conn
            .execute(
                "UPDATE trials SET outcome = 'accepted', cost_micros = 0,
                     cost_completeness = '\"unknown\"' WHERE trial_id = ?1",
                params![t.as_str()],
            )
            .expect("hand-written row");
        let error = ledger
            .trials_by_task(&source_task)
            .expect_err("a disagreeing pair is corrupt");
        assert!(matches!(error, LedgerError::Corrupt { .. }), "{error:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Settling is once, and a settlement that found no row is an error
    /// rather than a silent success.
    #[test]
    fn settling_an_unknown_or_already_settled_trial_is_refused() {
        let (ledger, dir) = temp_ledger();
        let source_task = TaskId::from_stored("task-d");
        let source_run = run("run-d");
        ledger
            .insert_run(&source_run, "/repo", None, &source_task, "rk")
            .expect("run");
        let t = TrialId::from_stored("trial-d");
        ledger
            .insert_trial(&new_trial(&t, &source_task, &source_run))
            .expect("insert trial");

        // A typo loses the settlement entirely if this returns Ok.
        let missing = TrialId::from_stored("no-such-trial");
        assert!(
            ledger
                .settle_trial(
                    &missing,
                    TrialOutcome::Accepted,
                    true,
                    TrialCost::UNKNOWN,
                    1
                )
                .is_err(),
            "settling a trial that does not exist must not report success"
        );

        ledger
            .settle_trial(&t, TrialOutcome::Accepted, true, TrialCost::UNKNOWN, 10)
            .expect("first settlement");
        assert!(
            ledger
                .settle_trial(&t, TrialOutcome::Rejected, false, TrialCost::UNKNOWN, 20)
                .is_err(),
            "a second settlement must not overwrite the first outcome"
        );
        let row = ledger
            .trials_by_task(&source_task)
            .expect("trials")
            .remove(0);
        assert_eq!(
            row.outcome,
            Some(TrialOutcome::Accepted),
            "the first outcome stands"
        );
        assert_eq!(row.duration_ms, Some(10), "and so does its duration");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An arm index that cannot be one is corrupt, not wrapped. `as u32`
    /// read a stored -1 back as 4294967295.
    #[test]
    fn an_out_of_range_arm_index_is_corrupt_not_wrapped() {
        let (ledger, dir) = temp_ledger();
        let source_task = TaskId::from_stored("task-e");
        let source_run = run("run-e");
        ledger
            .insert_run(&source_run, "/repo", None, &source_task, "rk")
            .expect("run");
        let t = TrialId::from_stored("trial-e");
        ledger
            .insert_trial(&new_trial(&t, &source_task, &source_run))
            .expect("insert trial");
        for stored in [-1i64, i64::from(u32::MAX) + 1] {
            ledger
                .conn
                .execute(
                    "UPDATE trials SET arm_index = ?2 WHERE trial_id = ?1",
                    params![t.as_str(), stored],
                )
                .expect("hand-written arm index");
            let error = ledger
                .trials_by_task(&source_task)
                .expect_err("an impossible arm index is corrupt");
            assert!(matches!(error, LedgerError::Corrupt { .. }), "{error:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A trial settled with an actual, known cost reads that cost back
    /// exactly — distinct from the unknown case above.
    #[test]
    fn settling_a_trial_with_a_known_cost_reads_it_back() {
        let (ledger, dir) = temp_ledger();
        let t = trial("trial-3");
        let source_task = task("t3");
        let source_run = run("run-3");
        ledger
            .insert_run(&source_run, "/repo", None, &source_task, "rk")
            .expect("run");
        ledger
            .insert_trial(&new_trial(&t, &source_task, &source_run))
            .expect("insert trial");
        ledger
            .settle_trial(
                &t,
                TrialOutcome::Rejected,
                false,
                TrialCost::new(
                    Some(MicroUsd::from_micros(123_456)),
                    CostCompleteness::Actual,
                )
                .expect("a figure with a completeness that describes it"),
                1_000,
            )
            .expect("settle");
        let row = ledger
            .trials_by_task(&source_task)
            .expect("trials")
            .remove(0);
        assert_eq!(row.outcome, Some(TrialOutcome::Rejected));
        assert_eq!(row.accepted_without_escalation, Some(false));
        let cost = row.cost.expect("a settled trial has a paired cost");
        assert_eq!(cost.cost(), Some(MicroUsd::from_micros(123_456)));
        assert_eq!(cost.completeness(), CostCompleteness::Actual);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn settled_trial_in(ledger: &Ledger, name: &str, outcome: Option<TrialOutcome>) -> TrialId {
        let id = trial(name);
        let source_task = task(name);
        let source_run = run(&format!("run-{name}"));
        ledger
            .insert_run(&source_run, "/repo", None, &source_task, "rk")
            .expect("run");
        ledger
            .insert_trial(&new_trial(&id, &source_task, &source_run))
            .expect("trial");
        if let Some(outcome) = outcome {
            ledger
                .settle_trial(&id, outcome, false, TrialCost::UNKNOWN, 1)
                .expect("settle");
        }
        id
    }

    #[test]
    fn judging_a_rejected_trial_records_verdict_note_and_time_once() {
        let (ledger, dir) = temp_ledger();
        let id = settled_trial_in(&ledger, "j1", Some(TrialOutcome::Rejected));
        let Ok(Judged::Recorded(truth)) = ledger
            .judge_trial(&id, GroundTruthVerdict::Pass, " same defect in abc123 ")
            .expect("reads")
        else {
            panic!("recorded")
        };
        assert_eq!(truth.verdict, GroundTruthVerdict::Pass);
        assert_eq!(truth.note, "same defect in abc123");
        assert!(!truth.at.is_empty());
        let stored = ledger.trial(&id).expect("reads").expect("row");
        assert_eq!(stored.ground_truth, Some(truth.clone()));

        // The same verdict again changes nothing.
        let again = ledger
            .judge_trial(&id, GroundTruthVerdict::Pass, "other words")
            .expect("reads");
        assert_eq!(again, Ok(Judged::AlreadyRecorded(truth.clone())));
        // The other verdict is refused and the first stands.
        let conflict = ledger
            .judge_trial(&id, GroundTruthVerdict::Fail, "no")
            .expect("reads");
        assert_eq!(conflict, Err(JudgeRefusal::Conflict(truth.clone())));
        assert_eq!(
            ledger.trial(&id).expect("reads").expect("row").ground_truth,
            Some(truth)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn judging_refuses_what_is_not_a_rejected_trial_with_a_note() {
        let (ledger, dir) = temp_ledger();
        assert_eq!(
            ledger
                .judge_trial(&trial("nope"), GroundTruthVerdict::Pass, "n")
                .expect("reads"),
            Err(JudgeRefusal::UnknownTrial)
        );
        for outcome in [
            Some(TrialOutcome::Accepted),
            Some(TrialOutcome::Errored),
            Some(TrialOutcome::Invalid),
            None,
        ] {
            let id = settled_trial_in(&ledger, &format!("o-{outcome:?}"), outcome);
            assert_eq!(
                ledger
                    .judge_trial(&id, GroundTruthVerdict::Pass, "n")
                    .expect("reads"),
                Err(JudgeRefusal::NotRejected(outcome))
            );
            assert_eq!(
                ledger.trial(&id).expect("reads").expect("row").ground_truth,
                None
            );
        }
        let id = settled_trial_in(&ledger, "empty", Some(TrialOutcome::Rejected));
        assert_eq!(
            ledger
                .judge_trial(&id, GroundTruthVerdict::Fail, "  ")
                .expect("reads"),
            Err(JudgeRefusal::EmptyNote)
        );
        assert_eq!(
            ledger.trial(&id).expect("reads").expect("row").ground_truth,
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
