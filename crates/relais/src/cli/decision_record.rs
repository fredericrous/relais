//! A recorded `relais decide` answer, and the plugin's proof that the answer
//! came through its decide tool. It stands apart from `decide` because the
//! modules `decide` calls also read it: kept inside `decide`, each of them
//! would import `decide` back, an import cycle
//! (`scripts/check-child-cycles.py crates/relais/src/cli`).

use super::ctx::node_error;
use super::ctx::refusal;
use super::ctx::Refusal;
use super::outcome::exit_code;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use super::programme::Format;
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
            format!("relais decide: no receipt written: no verification snapshot for this {reason} stop")
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

/// What a person does next after a held answer: the answers that close an
/// open decision, and `salvaged`, which may answer a run with none.
#[derive(Clone, Copy)]
pub(crate) enum NextStep {
    AnswerAgain,
    SalvageAgain,
}

/// A failure of an answer, as the person sees it. Another answerer holding
/// the decision is a refusal, exit 3, that says what to do next. The grant
/// says only that another answer holds the decision, not whether that
/// answer was recorded, nor on which node; the line says so and claims
/// nothing more (`relais status` reads this node's ledger alone).
/// `signed_off` is the criterion an approve recorded before the grant was
/// asked. Everything else is [`node_error`].
pub(crate) fn answer_error(
    e: NodeError,
    run: &RunId,
    signed_off: Option<&str>,
    next: NextStep,
) -> Result<Decision, CliError> {
    if !node::is_decision_held_elsewhere(&e, run) {
        return Err(node_error("decide", e));
    }
    let minutes = relais::shared_state::DECISION_GRANT_TTL_SECS / 60;
    let again = match next {
        NextStep::AnswerAgain => "answer again if it is still open".to_string(),
        NextStep::SalvageAgain => format!(
            "run `relais decide {run} --answer salvaged` again if this node should salvage it; \
             a salvage recorded on another node does not show here"
        ),
    };
    let mut message = format!(
        "another answer holds the decision of run {run} (it may already be recorded, here or \
         on another node): check `relais status {run}`, then, once the hold ends (up to \
         {minutes} minutes), {again}"
    );
    if let Some(criterion) = signed_off {
        message.push_str(&format!(
            "; the sign-off of criterion {criterion} is recorded"
        ));
    }
    Ok(refusal(CliOutcome::Blocked, message))
}

/// What a refused command prints, the one stream rule its callers share
/// (decisions ADR-0021: `cli.basics.stdout-is-data-stderr-is-talk`,
/// `cli.output.json-flag`):
///
/// - stdout carries what the command did: its result sentence, or under
///   `--json` exactly one object, the result or, for a refusal,
///   `{"error", "exit"}`;
/// - stderr carries every refusal, warning and failure as one line for the
///   person, `<command>: <message>`, under `--json` too;
/// - the exit code agrees with both.
///
/// An error that is not a refusal reaches stderr from `main`, as
/// `relais: <error>`: the prefix differs by path on purpose, a refusal
/// naming its command. Lives here, not in `outcome`, which `programme`
/// (owner of [`Format`]) imports.
///
/// Returns the stdout object (under `--json` only) and the stderr line.
pub(crate) fn refusal_streams(
    command: &str,
    format: Format,
    outcome: &CliOutcome,
    message: &str,
) -> (Option<String>, String) {
    let object = match format {
        Format::Json => Some(
            serde_json::json!({
                "error": message,
                "exit": exit_code(outcome),
            })
            .to_string(),
        ),
        Format::Text => None,
    };
    (object, format!("{command}: {message}"))
}

/// Print a refusal by [`refusal_streams`]. A reader that closed stdout is
/// not a failure (the exit code still says what happened); any other
/// failed write of the object is said in the one stderr line.
pub(crate) fn print_refusal(command: &str, format: Format, outcome: &CliOutcome, message: &str) {
    use std::io::Write;
    let (stdout, mut stderr) = refusal_streams(command, format, outcome, message);
    if let Some(object) = stdout {
        match writeln!(std::io::stdout().lock(), "{object}") {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => stderr.push_str(&format!(" (stdout: {e})")),
        }
    }
    let _ = writeln!(std::io::stderr().lock(), "{stderr}"); // nowhere left to say a failed write
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_one_stderr_line_and_under_json_also_one_stdout_object() {
        let (stdout, stderr) =
            refusal_streams("relais decide", Format::Json, &CliOutcome::Blocked, "held");
        let object: serde_json::Value =
            serde_json::from_str(&stdout.expect("an object under --json")).expect("json");
        assert_eq!(object["error"], "held");
        assert_eq!(object["exit"], exit_code(&CliOutcome::Blocked));
        assert_eq!(stderr, "relais decide: held");

        let (stdout, stderr) =
            refusal_streams("relais decide", Format::Text, &CliOutcome::Blocked, "held");
        assert_eq!(stdout, None, "nothing on stdout without --json");
        assert_eq!(stderr, "relais decide: held");
    }
}
