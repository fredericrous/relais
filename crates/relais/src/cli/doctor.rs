//! `relais doctor`, `relais hook` and `relais init`.

use super::ctx::hook_admission_settings;
use super::ctx::load_machine;
use super::ctx::load_repo_policy;
use super::ctx::print_document;
use super::ctx::project_dir;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use relais::backend::Harness;
use relais::doctor;
use relais::paths;
use relais::route;
use std::path::Path;

/// Say on doctor's `effort` finding which models a `raise` repair WOULD hold
/// because their effort order is unknown. Doctor has no task or budget, so
/// the line is conditional and lists every model a repair could run at;
/// `relais plan` states the models one run's ladder actually holds. Holding
/// the effort never changes the finding's level — nothing is blocked. A
/// repository or machine.toml that cannot be read says nothing here: doctor
/// reports those on findings of their own.
pub(crate) fn note_held_repair_effort(report: &mut doctor::DoctorReport) {
    let (Ok((_, repo)), Ok(machine)) = (load_repo_policy(), load_machine()) else {
        return;
    };
    let cli = relais::adapter::claude::ClaudeBackend::discover()
        .ok()
        .and_then(|backend| backend.probe())
        .map_or(relais::catalog::Fact::Unknown, |capabilities| {
            capabilities.accepted_efforts
        });
    let catalogs = relais::catalog::EffortCatalogs::resolve_all(
        &cli,
        &machine.efforts,
        &machine.routing.max_effort,
        repo.models.values().map(|profile| profile.id.as_str()),
    );
    let held = route::held_repair_models(&repo.models, &catalogs, repo.execution.repair_effort);
    if held.is_empty() {
        return;
    }
    if let Some(finding) = report
        .findings
        .iter_mut()
        .find(|finding| finding.component == "effort")
    {
        finding.detail.push('\n');
        finding.detail.push_str(&route::held_would_text(&held));
    }
}

/// What `relais doctor --json` prints: the findings, and the Claude Code
/// integration described apart from them.
#[derive(serde::Serialize)]
struct DoctorDocument<'a> {
    #[serde(flatten)]
    report: &'a doctor::DoctorReport,
    integration: relais::install::plugin::Integration,
}

pub(crate) fn doctor_command(json: bool) -> Result<CliOutcome, CliError> {
    use relais::install::plugin::Claude;
    let claude = Claude::discover();
    let listing = claude
        .as_ref()
        .map_err(Clone::clone)
        .and_then(Claude::installed);
    let integration = relais::install::plugin::integration(&claude, &listing);
    let mut report = doctor::doctor(&project_dir()?, listing);
    note_held_repair_effort(&mut report);
    if json {
        print_document(&DoctorDocument {
            report: &report,
            integration,
        })?;
    } else {
        print!("{}", report.render());
    }
    match report.failed() {
        true => Ok(CliOutcome::Blocked),
        false => Ok(CliOutcome::Accepted),
    }
}

/// `relais doctor --effort-template`: just the block to paste.
pub(crate) fn doctor_effort_template_command() -> Result<CliOutcome, CliError> {
    let (_, policy) = load_repo_policy()?;
    print!("{}", doctor::effort_template_for(&policy));
    Ok(CliOutcome::Accepted)
}

/// `relais hook --probe --record <dir>`: the record-only handler
/// for capturing hook payloads as fixtures. Reads one payload from
/// stdin, writes it verbatim, decides nothing, and always accepts — the
/// handler cannot fail the session it is watching.
pub(crate) fn hook_command(dir: &Path) -> Result<CliOutcome, CliError> {
    let order = relais::hook::arrival_nanos();
    relais::hook::record(dir, order, std::io::stdin().lock());
    Ok(CliOutcome::Accepted)
}

/// `relais hook`, run with no flags: read one payload on stdin, decide
/// what to say about it, and say it (SPEC §23). Always accepts — a
/// non-zero exit from a hook is reported to the session as a failure of
/// the tool call it was watching, so a relais that cannot answer must be
/// indistinguishable from a relais that had nothing to say. Every path
/// that could fail (an unreadable settings file, an unreachable
/// coordinator, a payload that is not JSON, a panic anywhere inside) is
/// swallowed rather than surfaced; `relais doctor` is where any of that
/// is reported as a finding.
pub(crate) fn hook_respond_command() -> CliOutcome {
    // The whole body, not just `respond::handle`: reading stdin,
    // loading settings and journalling all run here too, and none of
    // them may take the process down with them any more than the
    // decision itself may.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(hook_respond));
    CliOutcome::Accepted
}

pub(crate) fn hook_respond() {
    use std::io::Read;
    let mut payload = Vec::new();
    // A truncated read leaves `payload` with whatever arrived so far;
    // `event::parse` treats that the same as any other input it cannot
    // make sense of (`HookEvent::NotOurs`), so the hook still answers.
    let _ = std::io::stdin().lock().read_to_end(&mut payload);

    let settings = hook_admission_settings();

    let Ok(socket) = relais::coordinator::socket_path() else {
        // No home directory: nothing to connect to and nowhere to
        // journal. `decide_or_silent` still owes an answer for "the
        // coordinator could not be reached" under this machine's own
        // stance, so build one directly rather than skip the decision
        // entirely.
        let event = relais::hook::event::parse(&payload);
        let answer = relais::hook::decide::decide_or_silent(
            &event,
            &settings,
            None,
            std::time::Duration::ZERO,
        );
        if let Some(text) = answer.stdout_payload() {
            println!("{text}");
        }
        return;
    };
    let gate = relais::coordinator::RemoteGate::new(socket);
    let handled = relais::hook::respond::handle(&payload, &settings, &gate);
    if let Some(text) = handled.answer.stdout_payload() {
        println!("{text}");
    }
    if let Ok(path) = paths::hook_journal_path() {
        let entry = relais::hook::respond::journal_entry(&payload, &handled);
        // Best effort, like every other step here: a journal write
        // failure is not the tool call's to report, and it already
        // answered above.
        let _ = relais::hook::respond::append_journal(&path, &entry);
    }
}

pub(crate) fn init_command() -> Result<CliOutcome, CliError> {
    // At the repository root, not the shell's cwd: a policy written in a
    // subdirectory would govern nothing.
    let path = project_dir()?.join("relais.toml");
    match relais::repo::write_init_template(&path) {
        Ok(true) => {
            println!("{}", init_written_hint(&path));
            Ok(CliOutcome::Accepted)
        }
        Ok(false) => {
            eprintln!(
                "relais init: {} already exists; init never overwrites",
                path.display()
            );
            Ok(CliOutcome::InvalidInput)
        }
        Err(e) => {
            eprintln!("relais init: {e}");
            Ok(CliOutcome::OperationalFailure)
        }
    }
}

/// What a person does after init wrote a policy: review it, then grant.
pub(crate) fn init_written_hint(path: &Path) -> String {
    format!(
        "wrote {} (review it, then: relais trust show; relais trust grant --key <key> --reviewed-by <name>)",
        path.display()
    )
}

/// `relais init --detect`: propose a policy from the repository's files,
/// print it, and with `--write` write it — never over an existing one.
/// Nothing detected and nothing typed is its own exit code (19), with no
/// file written, so a caller can ask a person for the command.
pub(crate) fn init_detect_command(
    json: bool,
    write: bool,
    command: Option<&str>,
) -> Result<CliOutcome, CliError> {
    let root = project_dir()?;
    // Detection reads the root's listing; one that cannot be read is not
    // "nothing detected", which would send a person to type a command.
    if let Err(e) = std::fs::read_dir(&root) {
        eprintln!(
            "relais init --detect: {} could not be read: {e}",
            root.display()
        );
        return Ok(CliOutcome::OperationalFailure);
    }
    let mut proposal = relais::repo::detect_policy(&root);
    // Diagnostics, on stderr: stdout stays the proposal alone.
    for note in &proposal.notes {
        eprintln!(
            "relais init --detect: {}",
            relais::repo::escape_control(note)
        );
    }
    if let Some(text) = command {
        match relais::repo::parse_command_text(text) {
            Ok(argv) => proposal = proposal.with_command(argv, "--command"),
            Err(e) => {
                eprintln!("relais init --command: {e}");
                return Ok(CliOutcome::InvalidInput);
            }
        }
    }
    if proposal.is_empty() {
        if json {
            println!("{}", serde_json::json!({ "proposal": null }));
        } else {
            for line in skipped_lines(&proposal) {
                println!("{line}");
            }
        }
        eprintln!(
            "relais init --detect: no verification command found in {} \
             (no Makefile check/test target, Cargo.toml, go.mod, package.json \
             test script or pytest); name one with --command",
            root.display()
        );
        return Ok(CliOutcome::NothingDetected);
    }
    let path = root.join("relais.toml");
    if write {
        match relais::repo::write_new_policy(&path, &proposal.toml) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!(
                    "relais init: {} already exists; init never overwrites",
                    path.display()
                );
                return Ok(CliOutcome::InvalidInput);
            }
            Err(e) => {
                eprintln!("relais init: {e}");
                return Ok(CliOutcome::OperationalFailure);
            }
        }
    }
    if json {
        println!("{}", serde_json::json!({ "proposal": proposal }));
    } else {
        for line in proposal_lines(&proposal) {
            println!("{line}");
        }
    }
    if write {
        // A diagnostic: stdout carries the proposal alone, so it stays
        // one JSON document, or lines of at most 80 columns.
        eprintln!("{}", init_written_hint(&path));
    }
    Ok(CliOutcome::Accepted)
}

/// The proposal as plain lines: one per setup step and command, with its
/// source, then what was seen and not proposed. At most 80 columns where
/// the argv allows it: a source that does not fit goes on its own line.
/// Repository text is escaped, so a crafted name cannot draw a line.
pub(crate) fn proposal_lines(proposal: &relais::repo::Proposal) -> Vec<String> {
    const WIDTH: usize = 80;
    let mut lines = Vec::new();
    let rows = proposal
        .setup
        .iter()
        .map(|step| ("setup", step))
        .chain(proposal.commands.iter().map(|check| ("check", check)));
    for (kind, item) in rows {
        let argv = relais::repo::escape_control(&item.argv.join(" "));
        let source = relais::repo::escape_control(&item.source);
        let head = format!("  {kind}  {argv}");
        let one_line = format!("{head}   {source}");
        if one_line.chars().count() <= WIDTH {
            lines.push(one_line);
        } else {
            lines.push(head);
            lines.push(format!("         {source}"));
        }
    }
    lines.extend(skipped_lines(proposal));
    lines
}

pub(crate) fn skipped_lines(proposal: &relais::repo::Proposal) -> Vec<String> {
    proposal
        .skipped
        .iter()
        .map(|skip| {
            format!(
                "Skipped: {} ({})",
                relais::repo::escape_control(&skip.what),
                relais::repo::escape_control(&skip.reason)
            )
        })
        .collect()
}
