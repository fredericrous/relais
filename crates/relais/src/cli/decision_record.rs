//! A recorded `relais decide` answer, and the plugin's proof that the answer
//! came through its decide tool. It stands apart from `decide` because the
//! modules `decide` calls also read it: kept inside `decide`, each of them
//! would import `decide` back, an import cycle
//! (`scripts/check-child-cycles.py crates/relais/src/cli`).

use super::ctx::node_error;
use super::ctx::refusal;
use super::ctx::Refusal;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use relais::ids::RunId;
use relais::runner::Reason;
use relais::runner::State;
use relais::shared_state::node;
use relais::shared_state::node::NodeError;

/// What a recorded decision reports, for both the sentence and `--json`.
pub(crate) struct Decided {
    pub(crate) run: String,
    pub(crate) resolution: Reason,
    pub(crate) actor: relais::ledger::Actor,
    pub(crate) from_state: State,
    pub(crate) to_state: State,
    pub(crate) advice: serde_json::Value,
    pub(crate) sentence: String,
    /// The successor intent a `revise --widen-scope` or `--successor-
    /// contract` answer wrote (or, repeated, found).
    pub(crate) successor_intent: Option<serde_json::Value>,
    /// An approval that resolved and left the run with no receipt: the stop
    /// reason it answered, which saved no verification snapshot to seal one
    /// from. Printed as one stderr line, or as `receipt_skipped` under `--json`.
    pub(crate) receipt_skipped: Option<String>,
}

impl Decided {
    /// The stderr line for an approval that wrote no receipt.
    pub(crate) fn receipt_skipped_line(&self) -> Option<String> {
        self.receipt_skipped.as_ref().map(|reason| {
            format!("no receipt written: no verification snapshot for this {reason} stop")
        })
    }
}

pub(crate) type Decision = Result<Decided, Refusal>;

/// The plugin's proof on stdin (`--proof-stdin`).
#[derive(serde::Deserialize)]
pub(crate) struct DecideProof {
    pub(crate) session: String,
    pub(crate) nonce: String,
}

/// Read the `--proof-stdin` frame. A malformed frame is a refusal, never a
/// silent `cli`: the plugin wrote it, so a bad one is a bug to see.
pub(crate) fn read_decide_proof() -> Result<DecideProof, Refusal> {
    use std::io::Read;
    let mut text = String::new();
    if let Err(e) = std::io::stdin().take(64 * 1024).read_to_string(&mut text) {
        return refusal(
            CliOutcome::InvalidInput,
            format!("`--proof-stdin`: could not read stdin: {e}"),
        );
    }
    serde_json::from_str(text.trim()).or_else(|e| {
        refusal(
            CliOutcome::InvalidInput,
            format!("`--proof-stdin` expects {{\"session\", \"nonce\"}} on stdin: {e}"),
        )
    })
}

/// What a person does next after a held answer, for the answers that close
/// an open decision, and for `salvaged`, which may answer a run with none.
pub(crate) const ANSWER_AGAIN: &str = "answer again if it is still open";
pub(crate) const SALVAGE_AGAIN: &str =
    "run the same `--answer salvaged` again unless status shows it already salvaged";

/// A failure of an answer, as the person sees it. Another answerer holding
/// the decision is a refusal, exit 3, that says what to do next; the line
/// claims nothing about the decision's state, which was not read.
/// `signed_off` is the criterion an approve recorded before the grant was
/// asked. Everything else is [`node_error`].
pub(crate) fn answer_error(
    e: NodeError,
    run: &RunId,
    signed_off: Option<&str>,
    again: &str,
) -> Result<Decision, CliError> {
    use std::fmt::Write;
    if !node::is_decision_held_elsewhere(&e, run) {
        return Err(node_error("decide", e));
    }
    let minutes = relais::shared_state::DECISION_GRANT_TTL_SECS / 60;
    let mut message = format!(
        "the decision of run {run} is being answered elsewhere: check `relais status {run}`, \
         then {again} (the other answer holds it for up to {minutes} minutes)"
    );
    if let Some(criterion) = signed_off {
        let _ = write!(
            message,
            "; the sign-off of criterion {criterion} is recorded"
        );
    }
    Ok(refusal(CliOutcome::Blocked, message))
}
