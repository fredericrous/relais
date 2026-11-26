//! `relais doctor`, `relais hook` and `relais init`.

use super::ctx::load_repo_policy;
use super::ctx::machine_of;
use super::ctx::print_document;
use super::ctx::project_dir;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use relais::backend::Harness;
use relais::doctor;
use relais::paths;
use relais::route;
use relais::shared_state::SharedState;
use std::path::Path;

/// Say on doctor's `effort` finding which models a `raise` repair WOULD hold
/// because their effort order is unknown. Doctor has no task or budget, so
/// the line is conditional and lists every model a repair could run at;
/// `relais plan` states the models one run's ladder actually holds. Holding
/// the effort never changes the finding's level — nothing is blocked. A
/// repository or machine.toml that cannot be read says nothing here: doctor
/// reports those on findings of their own.
pub(crate) fn note_held_repair_effort(
    report: &mut doctor::DoctorReport,
    state: Option<&dyn SharedState>,
) {
    let (Ok((_, repo)), Some(Ok(machine))) = (load_repo_policy(), state.map(machine_of)) else {
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
    let Some(finding) = report
        .findings
        .iter_mut()
        .find(|finding| finding.component == "effort")
    else {
        return;
    };
    if !held.is_empty() {
        finding.detail.push('\n');
        finding.detail.push_str(&route::held_would_text(&held));
    }
}

/// The `loop_threshold` finding: always present when the repository policy
/// reads, whether or not an effort finding exists. An unreadable policy is
/// reported on findings of its own.
pub(crate) fn loop_threshold_finding() -> Option<doctor::Finding> {
    let (_, repo) = load_repo_policy().ok()?;
    Some(doctor::Finding {
        component: "loop_threshold",
        level: doctor::Level::Ok,
        detail: loop_threshold_text(repo.execution.loop_threshold),
    })
}

/// The `[execution] loop_threshold` line doctor prints.
fn loop_threshold_text(threshold: Option<u32>) -> String {
    match threshold {
        Some(repeats) => format!(
            "loop_threshold: {repeats} (a native worker repeating one failing command {repeats} times with no edit is stopped; a contract's limits.loop_threshold = 0 turns the guard off for its run)"
        ),
        None => "loop_threshold: unset (no worker is stopped; the guard only records what it would have stopped (shadow), and a contract's limits.loop_threshold = 0 turns even that off for its run)".to_string(),
    }
}

/// The `holdouts` finding: every holdout discovered for the current
/// repository with its source, attach rule and confirmation state, and the
/// store's unreachability when it cannot be read. Nothing when no store is
/// configured and machine.toml declares none for this repository. A
/// repository or machine.toml that cannot be read says nothing here: doctor
/// reports those on findings of their own.
pub(crate) fn holdouts_finding(state: Option<&dyn SharedState>) -> Option<doctor::Finding> {
    let (Ok(root), Some(Ok(machine))) = (project_dir(), state.map(machine_of)) else {
        return None;
    };
    let repo = relais::repo::main_checkout_name(&root).unwrap_or_default();
    let Ok(runs) = paths::runs_dir() else {
        return None;
    };
    let configured = machine.holdouts.store.is_some()
        || machine
            .holdouts
            .repos
            .get(&repo)
            .is_some_and(|named| !named.is_empty());
    if !configured {
        return None;
    }
    let view = match relais::holdout::discover(
        &machine.holdouts,
        &repo,
        &relais::runner::holdout_root(&runs).join("cache"),
        &format!("doctor-{}", std::process::id()),
    ) {
        Ok(view) => view,
        Err(invalid) => {
            return Some(doctor::Finding {
                component: "holdouts",
                level: doctor::Level::Fail,
                detail: format!("holdout_declaration_invalid: {invalid}"),
            });
        }
    };
    let store = machine.holdouts.store.clone().unwrap_or_default();
    let ledger = paths::ledger_path()
        .ok()
        .filter(|path| path.exists())
        .and_then(|path| relais::ledger::Ledger::open(&path).ok());
    let mut confirmed = std::collections::BTreeMap::new();
    if let Some(ledger) = &ledger {
        for name in view.declared.keys() {
            if let Ok(Some(held)) = ledger.holdout_confirmation(&store, &repo, name) {
                confirmed.insert(name.clone(), held);
            }
        }
    }
    let listed = relais::holdout::listing(&machine.holdouts, &repo, &view, &confirmed);
    let mut lines = Vec::new();
    let mut level = doctor::Level::Ok;
    if let Some(cause) = &view.unread {
        level = doctor::Level::Warn;
        lines.push(format!(
            "holdout_store_unreachable: {store} ({cause}); the holdouts it declares are unknown"
        ));
    }
    if listed.is_empty() && view.unread.is_none() {
        lines.push(format!("no holdout is declared for {repo}"));
    }
    for holdout in &listed {
        if matches!(
            holdout.state,
            relais::holdout::ConfirmationState::New | relais::holdout::ConfirmationState::Changed
        ) && level == doctor::Level::Ok
        {
            level = doctor::Level::Warn;
        }
        lines.push(format!(
            "{}  source {}  {}",
            holdout.name,
            holdout.source,
            holdout.state.as_str()
        ));
        lines.push(format!("  attach: {}", holdout.attach));
    }
    Some(doctor::Finding {
        component: "holdouts",
        level,
        detail: lines.join("\n"),
    })
}

/// The `purge` finding: whether the automatic prune keeps up (ended runs
/// past the prune age plus three days are the symptom, and so is a stamp
/// that is missing or stale while old runs wait), with the stamp's age, the
/// state directory's size and the effective prune age. Nothing when the
/// state directory is unknown. Deletes nothing.
pub(crate) fn purge_finding() -> Option<doctor::Finding> {
    use doctor::state_health as health;
    let state_dir = paths::state_dir().ok()?;
    let now = std::time::SystemTime::now();
    let env = super::read_auto_prune_env();
    let (setting, source) = relais::prune::auto_prune_setting(env.as_deref());
    let runs = match health::prune_age(&setting) {
        Some(age) => measure_runs(&state_dir, age, now),
        None => health::RunsHealth::NotChecked,
    };
    let verify = relais::runner::verify_root(&state_dir.join(paths::RUNS_DIR));
    Some(health::purge_finding(&health::PurgeFacts {
        setting,
        source,
        stamp: health::stamp_state(&state_dir, now),
        state_bytes: relais::prune::tree_bytes_except(&state_dir, &verify),
        runs,
    }))
}

/// The ended runs older than `age` the prune has not removed, or why that
/// could not be told. A state directory with no ledger has no runs to judge.
fn measure_runs(
    state_dir: &Path,
    age: std::time::Duration,
    now: std::time::SystemTime,
) -> doctor::state_health::RunsHealth {
    use doctor::state_health::{RunCounts, RunsHealth, PURGE_GRACE};
    use relais::prune::{KeepReason, RunVerdict};
    let unmeasured = |operation: String, error: String| RunsHealth::Unmeasured { operation, error };
    let ledger_path = match paths::ledger_path() {
        Ok(path) => path,
        Err(e) => return unmeasured("locating the ledger".to_string(), e.to_string()),
    };
    if !ledger_path.exists() {
        return RunsHealth::NotChecked;
    }
    let ledger = match relais::ledger::Ledger::open(&ledger_path) {
        Ok(ledger) => ledger,
        Err(e) => {
            return unmeasured(
                format!("opening the ledger {}", ledger_path.display()),
                e.to_string(),
            )
        }
    };
    let judge = relais::prune::RunJudge::load(&ledger, &state_dir.join(paths::BASELINE_CACHE_DIR));
    let runs_dir = state_dir.join(paths::RUNS_DIR);
    let old_runs = match relais::prune::plan_run_prune(&runs_dir, age, now, &|run, path| {
        judge.judge(run, path)
    }) {
        Ok(old_runs) => old_runs,
        Err(e) => {
            return unmeasured(
                format!("judging the runs in {}", runs_dir.display()),
                e.to_string(),
            )
        }
    };
    let mut counts = RunCounts::default();
    for dir in &old_runs {
        let overdue = dir.age >= age + PURGE_GRACE;
        match &dir.verdict {
            RunVerdict::Prunable { .. } => {
                counts.old_ended_runs += 1;
                if overdue {
                    counts.overdue_runs += 1;
                    counts.overdue_bytes += dir.bytes;
                }
            }
            RunVerdict::Kept(KeepReason::NoReceipt) if overdue => {
                counts.receiptless_runs += 1;
                counts.receiptless_bytes += dir.bytes;
            }
            RunVerdict::Kept(_) => {}
        }
    }
    RunsHealth::Measured(counts)
}

/// The `backups` note: the `ledger*` files beside the ledger, deleted by
/// nobody. Nothing when there are none.
pub(crate) fn backups_finding() -> Option<doctor::Finding> {
    use doctor::state_health as health;
    let state_dir = paths::state_dir().ok()?;
    health::backups_finding(
        &state_dir,
        &health::ledger_backups(&state_dir, std::time::SystemTime::now()),
    )
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
    // One state for every finding that reads the machine through it
    // (an-t29). One that does not build says nothing on those findings:
    // `shared_state_finding` below reports it.
    let shared = super::ctx::shared_state().ok();
    note_held_repair_effort(&mut report, shared.as_deref());
    report.findings.extend(loop_threshold_finding());
    report.findings.extend(holdouts_finding(shared.as_deref()));
    report.findings.extend(purge_finding());
    report.findings.extend(backups_finding());
    if let Ok(machine_toml) = relais::paths::machine_settings_path() {
        report.findings.extend(doctor::shared_state_finding(
            &machine_toml,
            super::ctx::EXTERNAL_CLIENT.is_some(),
        ));
    }
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

/// Write the hook's answer on stdout. A reader that has gone away
/// (`BrokenPipe`) is not a failure of the hook: the journal still follows.
fn print_hook_answer(text: &str) {
    use std::io::Write;
    if let Err(error) = writeln!(std::io::stdout().lock(), "{text}") {
        if error.kind() != std::io::ErrorKind::BrokenPipe {
            eprintln!("relais hook: cannot write the answer: {error}");
        }
    }
}

pub(crate) fn hook_respond() {
    use std::io::Read;
    relais::hook::respond::silence_gather_panics();
    let mut payload = Vec::new();
    // A truncated read leaves `payload` with whatever arrived so far;
    // `event::parse` treats that the same as any other input it cannot
    // make sense of (`HookEvent::NotOurs`), so the hook still answers.
    let _ = std::io::stdin().lock().read_to_end(&mut payload);

    let shared = super::ctx::shared_state_for_hook();
    let settings = super::ctx::hook_admission_settings_from(&shared.state);

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
            print_hook_answer(&text);
        }
        return;
    };
    let gate = relais::coordinator::RemoteGate::new(socket);
    let handled = relais::hook::respond::handle(&payload, &settings, &gate, &shared);
    if let Some(text) = handled.answer.stdout_payload() {
        print_hook_answer(&text);
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
        let checked = relais::repo::parse_command_text(text).and_then(|argv| {
            relais::repo::check_command_program(&argv, |program| program_exists(&root, program))
                .map(|()| argv)
        });
        match checked {
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

/// Whether `program` is a file in the repository, `node_modules/.bin/<name>`
/// or on this machine's `PATH`; an explicit path is looked up as given
/// (relative ones from the repository root).
fn program_exists(root: &std::path::Path, program: &str) -> bool {
    let path = std::path::Path::new(program);
    if path.is_absolute() {
        path.is_file()
    } else if path.components().count() > 1 {
        root.join(path).is_file()
    } else {
        root.join(path).is_file()
            || root.join("node_modules/.bin").join(path).is_file()
            || relais::tooling::which(program).is_some()
    }
}
