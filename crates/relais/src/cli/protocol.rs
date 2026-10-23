//! The `--protocol` pieces (SPEC §29): the channel, the refusal kept for
//! the `done` line, and the wire name of a failure.

use super::outcome::{exit_code, CliError, CliOutcome};
use super::{Command, DatasetCommand};

thread_local! {
    /// The refusal a `relais run --protocol` printed before any run
    /// existed, for the `done` line `main` sends in its place.
    static REFUSAL: std::cell::RefCell<Option<(&'static str, String)>> =
        const { std::cell::RefCell::new(None) };
}

/// Print a refusal that ends `relais run` before a run exists, and keep
/// it for the `done` line (see [`announce_unstarted`]).
pub(crate) fn refuse_run(code: &'static str, detail: String, outcome: CliOutcome) -> CliOutcome {
    eprintln!("relais run: blocked ({code}): {detail}");
    REFUSAL.with(|refusal| *refusal.borrow_mut() = Some((code, detail)));
    outcome
}

/// Every `relais run --protocol` (and `dataset replay --protocol`) that
/// exits sends one `done` line: the runner sends it for a run that
/// started; this sends it, with `run: null`, for one refused before it
/// could — no policy, unreadable settings, a bad contract, admission.
/// Without it the plugin had only an exit code to tell the model.
pub(crate) fn announce_unstarted(outcome: CliOutcome, error: Option<&CliError>) {
    if relais::protocol::done_sent() || outcome == CliOutcome::Accepted {
        return;
    }
    let kept = REFUSAL.with(|refusal| refusal.borrow_mut().take());
    let (code, detail) = match (error, kept) {
        (Some(e), _) => (e.wire_code(), e.to_string()),
        (None, Some((code, detail))) => (code, detail),
        (None, None) => (
            "refused",
            format!("relais exited with code {}", exit_code(&outcome)),
        ),
    };
    let _ = relais::protocol::Wire::process().send(&relais::protocol::Request::Done {
        run: None,
        outcome: "blocked",
        receipt: None,
        summary: None,
        trial: None,
        code: Some(code),
        detail: Some(&detail),
        successor_intent: None,
    });
}

/// The protocol channel of `relais run --protocol` (SPEC §29): installed
/// before the run starts, or refused on a platform that cannot.
pub(crate) fn open_protocol(
    command: &Command,
) -> Result<Option<relais::protocol::Channel>, CliError> {
    let run = matches!(command, Command::Run { protocol: true, .. });
    let replay = matches!(
        command,
        Command::Dataset {
            cmd: DatasetCommand::Replay { protocol: true, .. },
        }
    );
    if !run && !replay {
        return Ok(None);
    }
    let name = if replay { "dataset replay" } else { "run" };
    relais::protocol::install()
        .map(Some)
        .map_err(|e| CliError::Usage {
            detail: format!("{name} --protocol: {e}"),
        })
}

impl CliError {
    /// The name a `done` line gives this failure when it ends a
    /// `relais run --protocol` before any run exists (SPEC §29).
    pub(crate) fn wire_code(&self) -> &'static str {
        match self {
            CliError::Locate(relais::repo::LocateError::RepoWithoutPolicy(_)) => "no_policy",
            CliError::Locate(relais::repo::LocateError::NotInRepository(_)) => "not_in_repository",
            CliError::Read { what, .. } | CliError::Invalid { what, .. } => match *what {
                "the repository policy" => "invalid_policy",
                "the machine settings" => "invalid_machine_settings",
                _ => "invalid_input",
            },
            CliError::Home(_) | CliError::Cwd(_) => "environment",
            CliError::Usage { .. } => "refused",
            CliError::NoDataset { .. }
            | CliError::Amend(_)
            | CliError::AppendPolicy { .. }
            | CliError::Operational { .. } => "operational_failure",
        }
    }
}
