//! The session router's relais side (SPEC §30, plan
//! `docs/plans/2026-10-07-session-router.md`, wire contract
//! `docs/router-protocol.md`). The relais plugin routes the session's own
//! model; relais serves it what it needs to decide (`router-state`),
//! records what it decided and how tasks ended (`router-observe`), holds
//! the person's envelope (`router-envelope`) and the R3 gate (`router r3`),
//! and reports the spend (`relais report`).
//!
//! This file is the adapter: it reads machine.toml, the repository's
//! policy, the agent definitions, the environment and the ledger, and
//! hands values to the pure modules beside it (`table`, `mode`, `outcome`,
//! `holdout`, `stats`, `wire`, `pins`, `state`, `r3`, `report`). R1 has no
//! learner: `adjustments` is always empty.

pub mod holdout;
pub mod mode;
pub mod outcome;
pub mod pins;
pub mod r3;
pub mod report;
pub mod state;
pub mod stats;
pub mod table;
pub mod wire;

use std::path::{Path, PathBuf};

use crate::ledger::{Ledger, LedgerError, RouterProvenance};
use crate::policy::{MachineSettings, RepoPolicy, RoutingEnvelope};
use crate::trust::GrantError;

/// Where a write came from. The CLI cannot tell a person's shell from the
/// model's Bash call, so anything not the plugin's own question is
/// unattributed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    PluginAsk,
    Cli,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::PluginAsk => "plugin-ask",
            Source::Cli => "cli (unattributed)",
        }
    }
}

/// Why `router-state` could not answer at all.
#[derive(Debug)]
pub enum StateError {
    Home(crate::paths::HomeUnset),
    /// machine.toml exists and cannot be read or is invalid.
    Machine(String),
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::Home(e) => write!(f, "{e}"),
            StateError::Machine(detail) => write!(f, "machine.toml: {detail}"),
        }
    }
}

impl std::error::Error for StateError {}

/// machine.toml as settings; absent is the defaults.
pub fn read_machine(path: &Path) -> Result<MachineSettings, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => format!(
            "schema_version = {}\n",
            crate::policy::MACHINE_SCHEMA_VERSION
        ),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    MachineSettings::from_toml_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The repository root (for project agent definitions) and its policy,
/// when there is a valid one. An invalid `relais.toml` is said on stderr
/// and served as absent: the router falls back to the default tiers.
fn repo_context(cwd: &Path) -> (PathBuf, Option<RepoPolicy>) {
    use crate::repo::LocateError;
    match crate::repo::locate_repo_root(cwd) {
        Ok(root) => {
            let path = root.join("relais.toml");
            let policy = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| RepoPolicy::from_toml_str(&text).map_err(|e| e.to_string()));
            match policy {
                Ok(policy) => (root, Some(policy)),
                Err(e) => {
                    eprintln!(
                        "relais native router-state: warning: {} is not used ({e}); serving the default tiers",
                        path.display()
                    );
                    (root, None)
                }
            }
        }
        Err(LocateError::RepoWithoutPolicy(root)) => (root, None),
        Err(LocateError::NotInRepository(start)) => (start, None),
    }
}

/// The user-scope agent directory: `$CLAUDE_CONFIG_DIR/agents`, else
/// `$HOME/.claude/agents`.
fn user_agents_dir() -> Option<PathBuf> {
    match crate::paths::dir_override(crate::paths::CLAUDE_CONFIG_DIR_ENV) {
        Some(dir) => Some(dir.join("agents")),
        None => crate::paths::home_dir()
            .ok()
            .map(|home| home.join(".claude").join("agents")),
    }
}

/// Every pin the `*.md` definitions in `dirs` state, in directory order
/// (a later directory's pin for the same type wins). A file that cannot
/// be read or has no well-formed front matter is skipped with a warning.
pub fn definition_pins(dirs: &[PathBuf]) -> Vec<(String, String)> {
    let mut pins = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
            .collect();
        files.sort();
        for path in files {
            let stem = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| pins::parse_definition(&stem, &text).map_err(|e| e.0));
            match parsed {
                Ok(Some(pin)) => pins.push(pin),
                Ok(None) => {}
                Err(e) => eprintln!(
                    "relais native router-state: warning: agent definition {} skipped: {e}",
                    path.display()
                ),
            }
        }
    }
    pins
}

/// The latest R3 row and the completed tasks' token totals, read only when
/// the ledger already exists: `router-state` never creates one. A ledger
/// that cannot be read is said and served as empty, which keeps the mode
/// at shadow.
fn ledger_facts(path: &Path) -> (Option<state::R3Row>, Vec<(String, u8, u64)>) {
    if !path.exists() {
        return (None, Vec::new());
    }
    let read = Ledger::open(path).and_then(|ledger| {
        let r3 = ledger.latest_router_r3()?.and_then(|row| match row {
            RouterProvenance::R3 {
                id,
                passed,
                source,
                at,
            } => Some(state::R3Row {
                id,
                passed,
                at,
                source,
            }),
            RouterProvenance::Envelope { .. } => None,
        });
        Ok((r3, ledger.router_task_token_totals(&state::COMPLETED)?))
    });
    match read {
        Ok(facts) => facts,
        Err(e) => {
            eprintln!(
                "relais native router-state: warning: the ledger could not be read ({e}); no r3 pass and no observed priors"
            );
            (None, Vec::new())
        }
    }
}

/// `relais native router-state --session <id>`, run in `cwd`.
pub fn router_state(session: &str, cwd: &Path) -> Result<state::RouterState, StateError> {
    let machine_path = crate::paths::machine_settings_path().map_err(StateError::Home)?;
    let machine = read_machine(&machine_path).map_err(StateError::Machine)?;
    let (root, repo) = repo_context(cwd);
    let ledger_path = crate::paths::ledger_path().map_err(StateError::Home)?;
    let (r3, completed_task_tokens) = ledger_facts(&ledger_path);
    let mut dirs: Vec<PathBuf> = user_agents_dir().into_iter().collect();
    dirs.push(root.join(".claude").join("agents"));
    let env_mode = std::env::var(mode::MODE_ENV).ok();
    Ok(state::build(state::Inputs {
        session,
        machine: &machine,
        repo: repo.as_ref(),
        r3,
        env_mode: env_mode.as_deref(),
        completed_task_tokens,
        definition_pins: definition_pins(&dirs),
    }))
}

/// Why `router-observe` wrote nothing.
#[derive(Debug)]
pub enum ObserveError {
    /// Exit 2: the payload is wrong; retrying cannot help.
    Payload(wire::WireError),
    /// Exit 1: the ledger failed; the plugin retries.
    Ledger(LedgerError),
}

impl std::fmt::Display for ObserveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObserveError::Payload(e) => write!(f, "bad payload, nothing written: {e}"),
            ObserveError::Ledger(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ObserveError {}

/// Validate the whole payload, then write every record in one
/// transaction. The number of records written (duplicates included).
pub fn observe(ledger_path: &Path, payload: &str) -> Result<usize, ObserveError> {
    let batch = wire::parse_batch(payload).map_err(ObserveError::Payload)?;
    let ledger = Ledger::open(ledger_path).map_err(ObserveError::Ledger)?;
    ledger
        .record_router_batch(&batch.session, &batch.records)
        .map_err(ObserveError::Ledger)?;
    Ok(batch.records.len())
}

/// `text` with `[session_routing] envelope = { … }` set, everything else
/// as it was.
pub fn set_envelope(text: &str, envelope: &RoutingEnvelope) -> Result<String, GrantError> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| GrantError::Unwritable(e.to_string()))?;
    let table = doc
        .entry("session_routing")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| GrantError::Unwritable("`session_routing` is not a table".into()))?;
    let mut inline = toml_edit::InlineTable::new();
    inline.insert("granted_at", envelope.granted_at.clone().into());
    inline.insert("by", envelope.by.clone().into());
    inline.insert("epsilon_max", envelope.epsilon_max.into());
    inline.insert("source", envelope.source.clone().into());
    table.insert(
        "envelope",
        toml_edit::Item::Value(toml_edit::Value::InlineTable(inline)),
    );
    Ok(doc.to_string())
}

/// Write the envelope to machine.toml (locked, atomic, comments and mode
/// kept: the same writer as `trust grant`).
pub fn write_envelope(machine_path: &Path, envelope: &RoutingEnvelope) -> Result<(), GrantError> {
    crate::trust::edit_machine(machine_path, |text, _| {
        set_envelope(text, envelope).map(Some)
    })?;
    Ok(())
}

/// `relais report`'s section, or `None` when no routed task started in
/// the window.
pub fn report_section(
    ledger: &Ledger,
    since: &str,
    pricing: &crate::orchestration::PriceTable,
) -> Result<Option<report::SessionRoutingReport>, LedgerError> {
    let window = ledger.router_window(since)?;
    if window.tasks.is_empty() {
        return Ok(None);
    }
    Ok(Some(report::session_routing(&window, pricing)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> RoutingEnvelope {
        RoutingEnvelope {
            granted_at: "2026-10-07T00:00:00Z".into(),
            by: "me".into(),
            epsilon_max: 0.1,
            source: Source::PluginAsk.as_str().into(),
        }
    }

    #[test]
    fn the_envelope_is_set_and_everything_else_kept() {
        let before =
            "# mine\nschema_version = 1\n\n[session_routing]\n# tuned\nholdout_rate = 0.2\n";
        let after = set_envelope(before, &envelope()).unwrap();
        assert!(after.starts_with(before), "{after}");
        let machine = MachineSettings::from_toml_str(&after).unwrap();
        assert_eq!(machine.session_routing.envelope, Some(envelope()));
        assert_eq!(machine.session_routing.holdout_rate, 0.2);
        // Granting again replaces the envelope rather than adding one.
        let mut again = envelope();
        again.epsilon_max = 0.05;
        let twice = set_envelope(&after, &again).unwrap();
        let machine = MachineSettings::from_toml_str(&twice).unwrap();
        assert_eq!(machine.session_routing.envelope, Some(again));
    }

    #[test]
    fn write_envelope_keeps_comments_and_mode() {
        let dir = crate::test_support::temp_dir("router-envelope");
        let path = dir.join("machine.toml");
        let before = "# my machine\nschema_version = 1\n";
        std::fs::write(&path, before).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        write_envelope(&path, &envelope()).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(before), "{after}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o640);
        }
    }

    #[test]
    fn a_malformed_definition_is_skipped_and_the_rest_read() {
        let dir = crate::test_support::temp_dir("router-pins");
        let agents = dir.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("good.md"),
            "---\nname: good\nmodel: opus\n---\n",
        )
        .unwrap();
        std::fs::write(agents.join("bad.md"), "model: opus\n").unwrap();
        std::fs::write(agents.join("notes.txt"), "---\nmodel: opus\n---\n").unwrap();
        assert_eq!(
            definition_pins(&[agents, dir.join("absent")]),
            vec![("good".to_string(), "opus".to_string())]
        );
    }
}
