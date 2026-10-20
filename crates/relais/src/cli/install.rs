//! `relais install` and `relais uninstall`: what they plan, apply and print.

use super::ctx::hook_admission_settings;
use super::ctx::open_ledger;
use super::ctx::project_dir;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use relais::install::InstallReport;
use relais::install::InstallRequest;
use relais::install::Mode;
use relais::install::Scope;
use relais::paths;
use std::path::Path;
use std::path::PathBuf;
/// The request the two `--claude` commands share. `--write` is the only
/// thing that applies anything; `--user` is the only thing that leaves
/// the project.
pub(crate) fn install_request(write: bool, user: bool) -> Result<InstallRequest, CliError> {
    Ok(InstallRequest {
        scope: match user {
            true => Scope::User,
            false => Scope::Project(project_dir()?),
        },
        mode: match write {
            true => Mode::Apply,
            false => Mode::Preview,
        },
    })
}

/// The home directory an install request needs. Only the user scope
/// reads one; a project install must not be refused on a machine without
/// a home directory, so the project scope passes its own root (which
/// `InstallRequest::root` ignores) rather than resolving `HOME`.
pub(crate) fn install_home(scope: &Scope) -> Result<PathBuf, CliError> {
    match scope {
        Scope::User => paths::home_dir().map_err(CliError::Home),
        Scope::Project(dir) => Ok(dir.clone()),
    }
}

/// Print a plan and what applying it did. Returns the outcome: an apply
/// that could not carry out part of its plan exits non-zero and names the
/// files, rather than reporting "applied 0 change(s)" and 0 (C9).
pub(crate) fn render_install(
    verb: &str,
    request: &InstallRequest,
    report: &InstallReport,
    plugin: &relais::install::plugin::PluginReport,
) -> CliOutcome {
    println!(
        "relais {verb} --claude ({})\n{}{}",
        request.scope_label(),
        report.plan.render(),
        plugin.render()
    );
    let plugin_outcome = match &plugin.failure {
        None => CliOutcome::Accepted,
        Some(failure) => {
            eprintln!("relais {verb}: the plugin step failed — {failure}");
            CliOutcome::NotFullyApplied
        }
    };
    let files_outcome = match report.mode {
        Mode::Preview => {
            if report.plan.applicable_count() == 0 && plugin.steps.is_empty() {
                println!("nothing to do");
            } else {
                println!("preview only: re-run with --write to apply");
            }
            CliOutcome::Accepted
        }
        Mode::Apply => {
            println!("applied {} change(s)", report.applied.len());
            for action in &report.not_applied {
                eprintln!(
                    "relais {verb}: could not apply {} — it changed between the preview and \
                     --write; re-run to see what it is now",
                    action.relative().display()
                );
            }
            match report.not_applied.is_empty() {
                true => CliOutcome::Accepted,
                false => CliOutcome::NotFullyApplied,
            }
        }
    };
    worse_outcome(files_outcome, plugin_outcome)
}

/// The directory marketplace the plugin is installed from.
pub(crate) fn marketplace_dir() -> Result<PathBuf, CliError> {
    paths::claude_marketplace_dir().map_err(CliError::Home)
}

/// What an `install`/`uninstall --claude` invocation is aimed at. Not a
/// flag: `settings.json` is a file a person maintains by hand, so
/// touching it is a different target rather than a modifier of the same
/// one, and the two cases read as what they are at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Targets {
    /// The owned files under `.claude/` and the plugin.
    OwnedFiles,
    /// The owned files and the plugin, and then the hook wiring in
    /// `settings.json`.
    OwnedFilesAndHooks,
}

impl Targets {
    /// `--hooks` names the second target. The flag lives in the CLI
    /// surface, where clap parses it; nothing below this line takes a
    /// bool that means "and also do the other thing".
    pub(crate) fn from_flag(hooks: bool) -> Self {
        match hooks {
            true => Targets::OwnedFilesAndHooks,
            false => Targets::OwnedFiles,
        }
    }
}

pub(crate) fn install_command(
    write: bool,
    user: bool,
    targets: Targets,
) -> Result<CliOutcome, CliError> {
    let request = install_request(write, user)?;
    let home = install_home(&request.scope)?;
    let report = operational(relais::install::install(&request, &home), "install")?;
    let plugin = relais::install::plugin::install(request.mode, &marketplace_dir()?);
    let outcome = render_install("install", &request, &report, &plugin);
    if request.mode == Mode::Apply && plugin.failure.is_none() {
        install_routing_envelope();
        install_learning_envelope();
    }
    match targets {
        Targets::OwnedFiles => Ok(outcome),
        Targets::OwnedFilesAndHooks => Ok(worse_outcome(
            outcome,
            install_hooks_target(&request, &home)?,
        )),
    }
}

/// Installing the plugin is the person's authorization to route the
/// session (plan, decision 1, R1b): record the envelope when machine.toml
/// has neither one nor an `off` tombstone, and say how to turn it off. A
/// failure here is said and does not fail the install: routing then stays
/// in shadow.
pub(crate) fn install_routing_envelope() {
    let Ok(machine_path) = paths::machine_settings_path() else {
        eprintln!("relais install: session routing not recorded: no config directory");
        return;
    };
    let envelope = relais::policy::RoutingEnvelope {
        granted_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        by: "relais install".to_string(),
        epsilon_max: 0.1,
        source: relais::router::Source::Install.as_str().to_string(),
    };
    match relais::router::write_envelope_if_absent(&machine_path, &envelope) {
        Ok(true) => {
            let recorded = open_ledger().and_then(|ledger| {
                operational(
                    ledger.record_router_provenance(&relais::ledger::RouterProvenance::Envelope {
                        by: envelope.by.clone(),
                        epsilon_max: envelope.epsilon_max,
                        source: envelope.source.clone(),
                        at: envelope.granted_at.clone(),
                    }),
                    "recording the envelope in the ledger",
                )
            });
            if let Err(e) = recorded {
                eprintln!("relais install: warning: the ledger did not record the envelope ({e})");
            }
            println!(
                "session routing is on; turn it off with /relais-routing off or RELAIS_SESSION_ROUTING=off"
            );
        }
        Ok(false) => {}
        Err(e) => eprintln!("relais install: session routing not recorded: {e}"),
    }
}

/// Installing the plugin is also the person's authorization for automatic
/// promotion of a task artifact that passed every gate (relais ADR-0003):
/// record the grant when machine.toml has neither one nor an `off`
/// tombstone, and say how to turn it off. A failure is said and does not
/// fail the install: the cycle then stops at `awaiting_promotion`.
pub(crate) fn install_learning_envelope() {
    let Ok(machine_path) = paths::machine_settings_path() else {
        eprintln!("relais install: automatic promotion not recorded: no config directory");
        return;
    };
    let grant = relais::policy::AutoPromote {
        granted_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        by: "relais install".to_string(),
        source: relais::router::Source::Install.as_str().to_string(),
    };
    match relais::learn::envelope::write_auto_promote_if_absent(&machine_path, &grant) {
        Ok(true) => println!("automatic promotion is on; turn it off with /relais-learning off"),
        Ok(false) => {}
        Err(e) => eprintln!("relais install: automatic promotion not recorded: {e}"),
    }
}

/// The settings.json half of `install --claude --hooks`, on its own so
/// the function above stays one thing: the owned files, then whatever
/// else was named.
pub(crate) fn install_hooks_target(
    request: &InstallRequest,
    home: &Path,
) -> Result<CliOutcome, CliError> {
    let relais_binary = operational(std::env::current_exe(), "install --hooks")?;
    let root = request.root(home);
    let queue_wait = admission_queue_wait();
    // Both roots the harness merges, regardless of which one this install
    // writes to: a `--user` install still has to see the project's files,
    // or it writes the second handler issue #94 is about. The project is
    // the directory the command was run in; `home` here is the install
    // scope's root, which for a project install is the project itself, so
    // the real home is resolved separately.
    let project = operational(std::env::current_dir(), "install --hooks")?;
    let real_home = relais::paths::home_dir().ok();
    let roots = relais::install::settings::MergedRoots {
        project: Some(project.as_path()),
        home: real_home.as_deref(),
    };
    match request.mode {
        Mode::Preview => Ok(render_hooks_preview(operational(
            root.plan_hooks(&relais_binary, queue_wait, roots),
            "install --hooks",
        )?)),
        Mode::Apply => {
            let applied = operational(
                root.apply_hooks(&relais_binary, queue_wait, roots),
                "install --hooks",
            )?;
            Ok(render_hooks_applied(applied))
        }
    }
}

/// The admission wait the installer must size the `PreToolUse` handler
/// timeout for — a missing or unreadable machine.toml is exactly the
/// default this crate ships (`hook_admission_settings` reads the same
/// way), never an error install itself should stop over.
pub(crate) fn admission_queue_wait() -> std::time::Duration {
    match hook_admission_settings().queue_behaviour() {
        relais::policy::QueueBehaviour::RefuseImmediately => std::time::Duration::ZERO,
        relais::policy::QueueBehaviour::WaitUpTo(wait) => wait,
    }
}

pub(crate) fn uninstall_command(
    write: bool,
    user: bool,
    targets: Targets,
) -> Result<CliOutcome, CliError> {
    let request = install_request(write, user)?;
    let home = install_home(&request.scope)?;
    let report = operational(relais::install::uninstall(&request, &home), "uninstall")?;
    let plugin = relais::install::plugin::uninstall(request.mode, &marketplace_dir()?);
    let outcome = render_install("uninstall", &request, &report, &plugin);
    match targets {
        Targets::OwnedFiles => Ok(outcome),
        Targets::OwnedFilesAndHooks => Ok(worse_outcome(
            outcome,
            uninstall_hooks_target(&request, &home)?,
        )),
    }
}

/// The settings.json half of `uninstall --claude --hooks`.
pub(crate) fn uninstall_hooks_target(
    request: &InstallRequest,
    home: &Path,
) -> Result<CliOutcome, CliError> {
    let relais_binary = operational(std::env::current_exe(), "uninstall --hooks")?;
    let root = request.root(home);
    match request.mode {
        Mode::Preview => Ok(render_hooks_removal_preview(operational(
            root.plan_hooks_removal(&relais_binary),
            "uninstall --hooks",
        )?)),
        Mode::Apply => {
            let removed = operational(
                root.apply_hooks_removal(&relais_binary),
                "uninstall --hooks",
            )?;
            Ok(render_hooks_removed(removed))
        }
    }
}

/// The worse of two outcomes from one combined `--claude --hooks`
/// invocation, so a hooks refusal is never masked by an otherwise clean
/// file install (or the reverse). `Accepted` is the only outcome neither
/// side reports as a problem, so anything else wins.
pub(crate) fn worse_outcome(a: CliOutcome, b: CliOutcome) -> CliOutcome {
    if a == CliOutcome::Accepted {
        b
    } else {
        a
    }
}

/// Print what wiring the hook would do, in preview mode.
pub(crate) fn render_hooks_preview(plan: relais::install::HooksPlan) -> CliOutcome {
    match plan {
        relais::install::HooksPlan::Ready { events } => {
            println!("relais install --claude --hooks (settings.json)");
            for event in &events {
                println!(
                    "  {:<8} {}",
                    match event.action {
                        relais::install::HookEventAction::Current => "keep",
                        relais::install::HookEventAction::CorrectTimeout => "retime",
                        relais::install::HookEventAction::UpdatePath => "repoint",
                        relais::install::HookEventAction::MigrateMatcher => "migrate",
                        relais::install::HookEventAction::RemoveRetired => "remove",
                        relais::install::HookEventAction::JoinExisting => "join",
                        relais::install::HookEventAction::NewEntry => "add",
                    },
                    event.event
                );
            }
            if events.iter().any(|e| e.action.changes_anything()) {
                println!("preview only: re-run with --write to apply");
            } else {
                println!("nothing to do");
            }
            CliOutcome::Accepted
        }
        relais::install::HooksPlan::Unrenderable {
            reason,
            paste_block,
        } => print_unrenderable("install", &reason, &paste_block),
    }
}

pub(crate) fn render_hooks_applied(applied: relais::install::HooksApplied) -> CliOutcome {
    match applied {
        relais::install::HooksApplied::AlreadyCurrent => {
            println!("relais install --claude --hooks: settings.json is already current");
            CliOutcome::Accepted
        }
        relais::install::HooksApplied::Applied(events) => {
            println!(
                "relais install --claude --hooks: wired {} event(s): {}",
                events.len(),
                events.join(", ")
            );
            CliOutcome::Accepted
        }
        relais::install::HooksApplied::Refused {
            reason,
            paste_block,
        } => print_unrenderable("install", &reason, &paste_block),
    }
}

pub(crate) fn render_hooks_removal_preview(
    plan: relais::install::settings::HooksRemovalPlan,
) -> CliOutcome {
    match plan {
        relais::install::settings::HooksRemovalPlan::Ready { events } => {
            println!("relais uninstall --claude --hooks (settings.json)");
            for event in &events {
                println!(
                    "  {:<8} {}",
                    match event.action {
                        relais::install::settings::HookRemovalAction::WouldRemove => "remove",
                        relais::install::settings::HookRemovalAction::NotPresent => "keep",
                    },
                    event.event
                );
            }
            if events
                .iter()
                .any(|e| e.action == relais::install::settings::HookRemovalAction::WouldRemove)
            {
                println!("preview only: re-run with --write to apply");
            } else {
                println!("nothing to do");
            }
            CliOutcome::Accepted
        }
        relais::install::settings::HooksRemovalPlan::Unrenderable { reason } => {
            eprintln!("relais uninstall --hooks: refused — {reason}");
            CliOutcome::Blocked
        }
    }
}

pub(crate) fn render_hooks_removed(removed: relais::install::HooksRemoved) -> CliOutcome {
    match removed {
        relais::install::HooksRemoved::AlreadyAbsent => {
            println!("relais uninstall --claude --hooks: nothing of relais's was on settings.json");
            CliOutcome::Accepted
        }
        relais::install::HooksRemoved::Removed(events) => {
            println!(
                "relais uninstall --claude --hooks: removed {} event(s): {}",
                events.len(),
                events.join(", ")
            );
            CliOutcome::Accepted
        }
        relais::install::HooksRemoved::Refused { reason } => {
            eprintln!("relais uninstall --hooks: refused — {reason}");
            CliOutcome::Blocked
        }
    }
}

/// A settings.json that failed the round-trip check: nothing was
/// written, so the exit is `NotFullyApplied` rather than `Blocked` —
/// the file install half of a combined `--claude --hooks` run may well
/// have succeeded, and this is "part of the plan could not be applied",
/// not "nothing ran at all".
pub(crate) fn print_unrenderable(verb: &str, reason: &str, paste_block: &str) -> CliOutcome {
    eprintln!("relais {verb} --hooks: refused — {reason}");
    // Nothing to paste when the reason already says what to change by hand.
    if !paste_block.is_empty() {
        println!("paste this into settings.json's \"hooks\" key by hand:\n{paste_block}");
    }
    CliOutcome::NotFullyApplied
}
