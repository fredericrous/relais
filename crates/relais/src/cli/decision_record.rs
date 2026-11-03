//! What a recorded `relais decide` answer reports, and the plugin's proof
//! that the answer came through its decide tool. `decide`, `feedback` and
//! `salvage` all read these; keeping them out of `decide` keeps the three
//! acyclic (`scripts/check-child-cycles.py crates/relais/src/cli`).

use super::ctx::refusal;
use super::ctx::Refusal;
use super::outcome::CliOutcome;
use relais::runner::Reason;
use relais::runner::State;

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
