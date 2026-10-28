//! `relais trust`: showing and recording the repository grant in machine.toml.

use super::ctx::load_machine;
use super::ctx::load_repo_policy;
use super::ctx::open_ledger;
use super::ctx::print_document;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use super::plan::pending_command_change;
use super::GrantScope;
use super::GrantSource;
use super::TrustCommand;
use relais::ledger::Ledger;
use relais::paths;
use relais::policy::RepoIdentity;
use relais::policy::RepoPolicy;
pub(crate) fn trust_command(cmd: TrustCommand) -> Result<CliOutcome, CliError> {
    let (root, policy) = load_repo_policy()?;
    let machine = load_machine()?;
    let machine_path = paths::machine_settings_path().map_err(CliError::Home)?;
    let identity = relais::repo::identity(&root);
    let repo_key = relais::policy::repo_key(&identity);
    match cmd {
        TrustCommand::Show { json } => {
            // Read-only: the ledger is consulted only if it already
            // exists, so `show` never creates the state directory.
            let previous = previous_grant_steps(&repo_key);
            let mut shown = relais::trust::show(
                &policy,
                &identity,
                &machine,
                machine_path,
                previous.as_deref(),
            );
            shown.pending_change = pending_command_change("trust show", &repo_key, &policy);
            if json {
                print_document(&shown)?;
            } else {
                print!("{}", trust_show_text(&shown));
            }
            Ok(CliOutcome::Accepted)
        }
        TrustCommand::Grant {
            key,
            reviewed_by,
            note,
            source,
            json,
        } => {
            let shown =
                relais::trust::show(&policy, &identity, &machine, machine_path.clone(), None);
            if reviewed_by.trim().is_empty() {
                eprintln!(
                    "relais trust grant: --reviewed-by is empty; name who reviewed the commands"
                );
                return Ok(CliOutcome::InvalidInput);
            }
            let record = relais::policy::TrustGrant {
                granted_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                reviewed_by: reviewed_by.clone(),
                note,
                repo: Some(identity.label().to_string()),
            };
            let (granted, recorded_key, scope) = match key {
                None => (
                    relais::trust::grant_repository(&machine_path, &repo_key, &record),
                    repo_key.clone(),
                    GrantScope::Repository,
                ),
                Some(key) => {
                    if key != shown.grant_key {
                        if json {
                            print_document(&serde_json::json!({
                                "code": "stale_grant_key",
                                "current_key": shown.grant_key,
                            }))?;
                        }
                        eprintln!(
                            "relais trust grant: refused (stale_grant_key): {key} is not this \
                             policy's key; relais.toml changed since it was shown. The current \
                             key is {}; review it with `relais trust show`",
                            shown.grant_key
                        );
                        return Ok(CliOutcome::StaleGrantKey);
                    }
                    eprintln!(
                        "relais trust grant: --key is deprecated: it grants this exact policy \
                         only. `relais trust grant --reviewed-by <name>` trusts the repository \
                         once"
                    );
                    (
                        relais::trust::grant(&machine_path, &key, &record),
                        key,
                        GrantScope::Legacy,
                    )
                }
            };
            let granted = match granted {
                Ok(granted) => granted,
                Err(e) => {
                    eprintln!("relais trust grant: {e}");
                    return Ok(CliOutcome::OperationalFailure);
                }
            };
            // The grant is in machine.toml now; the ledger row is its audit
            // trail and the base of the next question's change marks, and a
            // repository grant also starts the declaration the command
            // warning compares against. A ledger that cannot take it is
            // said, not turned into a failed grant: the grant stands
            // either way.
            if granted == relais::trust::Granted::Written {
                if let Err(e) = record_grant(&GrantRecord {
                    shown: &shown,
                    key: &recorded_key,
                    repo_key: &repo_key,
                    identity: &identity,
                    reviewed_by: &reviewed_by,
                    source,
                    scope,
                    policy: &policy,
                }) {
                    eprintln!(
                        "relais trust grant: warning: the grant is in machine.toml, but the ledger \
                         did not record it ({e}); the next question will not mark what changed"
                    );
                }
            }
            let written = granted == relais::trust::Granted::Written;
            if json {
                print_document(&serde_json::json!({
                    "code": if written { "granted" } else { "already_granted" },
                    "scope": scope.as_str(),
                    "grant_key": recorded_key,
                    "machine_settings": machine_path,
                }))?;
            } else if written {
                println!(
                    "trust grant: {} {recorded_key} recorded in {}",
                    scope.as_str(),
                    machine_path.display()
                );
            } else {
                println!(
                    "trust grant: {} {recorded_key} was already in {}",
                    scope.as_str(),
                    machine_path.display()
                );
            }
            Ok(CliOutcome::Accepted)
        }
    }
}

/// The steps the latest recorded grant for this repository authorized,
/// when the ledger exists and holds one. Best effort: a ledger that
/// cannot be read only costs the change marks.
pub(crate) fn previous_grant_steps(repo_key: &str) -> Option<Vec<relais::trust::Step>> {
    let path = paths::ledger_path().ok()?;
    if !path.exists() {
        return None;
    }
    let read = Ledger::open(&path)
        .map_err(|e| e.to_string())
        .and_then(|ledger| {
            ledger
                .latest_trust_grant_steps(repo_key)
                .map_err(|e| e.to_string())
        })
        .and_then(|json| match json {
            Some(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|e| e.to_string()),
            None => Ok(None),
        });
    match read {
        Ok(steps) => steps,
        Err(e) => {
            eprintln!(
                "relais trust show: warning: the last grant could not be read from the ledger ({e}); \
                 changes are not marked"
            );
            None
        }
    }
}

/// The ledger row of a grant just written: who, from where, and the steps
/// it authorized.
pub(crate) struct GrantRecord<'a> {
    pub(crate) shown: &'a relais::trust::Shown,
    pub(crate) key: &'a str,
    pub(crate) repo_key: &'a str,
    pub(crate) identity: &'a RepoIdentity,
    pub(crate) reviewed_by: &'a str,
    pub(crate) source: GrantSource,
    pub(crate) scope: GrantScope,
    pub(crate) policy: &'a RepoPolicy,
}

pub(crate) fn record_grant(grant: &GrantRecord<'_>) -> Result<(), CliError> {
    let GrantRecord {
        shown,
        key,
        repo_key,
        identity,
        reviewed_by,
        source,
        scope,
        policy,
    } = *grant;
    let steps = operational(
        serde_json::to_string(&shown.declaration.steps),
        "rendering the granted steps",
    )?;
    let ledger = open_ledger()?;
    operational(
        ledger.record_trust_grant(&relais::ledger::TrustGrantRecord {
            grant_key: key,
            repo_key,
            repo: identity.label(),
            reviewed_by,
            source: source.as_str(),
            steps_json: &steps,
        }),
        "recording the grant in the ledger",
    )?;
    match scope {
        // A repository grant starts the declaration the command warning
        // compares against, so the first change after it is the first said.
        GrantScope::Repository => {
            use relais::trust::DeclarationStore;
            operational(
                relais::trust::Recorder(&ledger)
                    .record(repo_key, &relais::trust::commands_of(policy))
                    .map_err(std::io::Error::other),
                "recording the starting declaration in the ledger",
            )
        }
        // A legacy grant trusts one exact policy; the commands it covers
        // are its key, so there is no declaration to start.
        GrantScope::Legacy => Ok(()),
    }
}

/// `relais trust show` for a person: the key on a line of its own, then
/// the commands with their change marks, the models and integrations.
pub(crate) fn trust_show_text(shown: &relais::trust::Shown) -> String {
    use relais::trust::{display_argv, escape_display, Change};
    let mut out = String::new();
    match &shown.repository_grant {
        Some(grant) => out.push_str(&format!(
            "repository: granted {} by {}\n",
            escape_display(&grant.granted_at),
            escape_display(&grant.reviewed_by)
        )),
        None => out.push_str("repository: not granted\n"),
    }
    out.push_str(&format!(
        "identity: {}\n",
        escape_display(&shown.repository)
    ));
    out.push_str(&format!("grant key (legacy): {}\n", shown.grant_key));
    out.push_str(&format!(
        "status: {}\n",
        match (&shown.repository_grant, shown.granted) {
            (Some(_), _) => "granted (repository)",
            (None, true) => "granted (legacy grant for this exact policy)",
            (None, false) => "not granted",
        }
    ));
    out.push_str(&format!(
        "machine.toml: {} ($RELAIS_CONFIG_DIR, else ~/.config/relais)\n",
        shown.machine_settings.display()
    ));
    out.push_str("commands relais would run:\n");
    let width = shown
        .declaration
        .steps
        .iter()
        .map(|step| step.kind.as_str().len())
        .max()
        .unwrap_or(5);
    for (i, step) in shown.declaration.steps.iter().enumerate() {
        let mark = match shown.changes.as_ref().map(|changes| changes[i]) {
            Some(Change::New) => '+',
            Some(Change::Changed) => '~',
            _ => ' ',
        };
        out.push_str(&format!(
            "{mark} {:width$}  {}  [{}]\n",
            step.kind.as_str(),
            display_argv(&step.argv),
            escape_display(&step.profile),
        ));
    }
    let models: Vec<String> = shown
        .declaration
        .models
        .iter()
        .map(|model| format!("{} {}", model.tier, escape_display(&model.id)))
        .collect();
    out.push_str(&format!("models: {}\n", models.join(" · ")));
    let integrations: Vec<String> = shown
        .declaration
        .integrations
        .iter()
        .map(|line| format!("{} ({})", line.name, line.mode))
        .collect();
    out.push_str(&format!(
        "integrations: {}\n",
        if integrations.is_empty() {
            "none".to_string()
        } else {
            integrations.join(" · ")
        }
    ));
    if let Some(change) = &shown.pending_change {
        out.push_str(&format!("{}\n", change.text()));
    }
    out
}
