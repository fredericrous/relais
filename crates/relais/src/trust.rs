//! Trust grants issued by a command (SPEC §5): what a grant would
//! authorize, in a form a person can read and a plugin can put to them,
//! and the one writer of the machine settings' `[trust]` and
//! `[trust_repos]` tables.
//!
//! A grant is executable authority, so this module never decides to
//! issue one. `relais trust grant` is run by a person in a shell, or by
//! the plugin after the person answered its question; no run, recipe or
//! worker reaches [`grant`] or [`grant_repository`].
//!
//! A repository is trusted once (ADR-0007). What a later change to its
//! commands does is not a block but a warning, said once per change: the
//! [`Commands`] a repository last ran under are kept by a
//! [`DeclarationStore`], and [`observe`] compares them with the current
//! ones.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ledger::Ledger;
use crate::policy::{
    grant_key, repo_key, MachineSettings, PolicyError, RepoIdentity, RepoPolicy, TrustGrant,
    MACHINE_SCHEMA_VERSION,
};

/// What one step of a verification profile is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// Installs what the commands need (`npm ci`). Runs first.
    Setup,
    /// A command whose exit status is a verdict.
    Check,
}

impl StepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StepKind::Setup => "setup",
            StepKind::Check => "check",
        }
    }
}

/// One command a grant would let relais run, exactly as it would run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub profile: String,
    pub kind: StepKind,
    pub argv: Vec<String>,
}

/// One model tier the policy routes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelLine {
    pub tier: &'static str,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// One integration and its mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationLine {
    pub name: &'static str,
    pub mode: &'static str,
}

/// What a person is shown before granting: every command, the models,
/// the integrations. The rest of the policy (risk paths, recipes,
/// execution limits) is hashed into the grant too, but it runs nothing
/// on its own; `relais.toml` itself is where it is reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Declaration {
    pub steps: Vec<Step>,
    pub models: Vec<ModelLine>,
    pub integrations: Vec<IntegrationLine>,
}

/// Setup first, then checks, profile by profile (profiles are a sorted
/// map, so the order is stable).
pub fn declaration(policy: &RepoPolicy) -> Declaration {
    let mut steps = Vec::new();
    for (profile, spec) in &policy.verification.profiles {
        for (kind, commands) in [
            (StepKind::Setup, &spec.setup),
            (StepKind::Check, &spec.commands),
        ] {
            steps.extend(commands.iter().map(|command| Step {
                profile: profile.clone(),
                kind,
                argv: command.argv.clone(),
            }));
        }
    }
    let models = policy
        .models
        .iter()
        .map(|(tier, model)| ModelLine {
            tier: tier.as_str(),
            id: model.id.clone(),
            effort: model
                .effort
                .as_ref()
                .map(|effort| effort.as_str().to_string()),
        })
        .collect();
    let integrations = [
        ("aval", &policy.integrations.aval),
        ("amont", &policy.integrations.amont),
        ("amont_agent", &policy.integrations.amont_agent),
    ]
    .into_iter()
    .filter_map(|(name, dependency)| {
        dependency.as_ref().map(|dependency| IntegrationLine {
            name,
            mode: match dependency.mode() {
                crate::policy::DependencyMode::Required => "required",
                crate::policy::DependencyMode::Optional => "optional",
                crate::policy::DependencyMode::Off => "off",
            },
        })
    })
    .collect();
    Declaration {
        steps,
        models,
        integrations,
    }
}

/// What the command-change warning watches: the steps and the integrations
/// of the [`Declaration`], never its models, so a routing candidate (which
/// only changes models and recipes) never warns. Execution limits are not
/// covered either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commands {
    pub steps: Vec<Step>,
    /// `name (mode)`, one per configured integration.
    pub integrations: Vec<String>,
}

pub fn commands_of(policy: &RepoPolicy) -> Commands {
    let declaration = declaration(policy);
    Commands {
        steps: declaration.steps,
        integrations: declaration
            .integrations
            .iter()
            .map(|line| format!("{} ({})", line.name, line.mode))
            .collect(),
    }
}

/// The hash of [`Commands`]: equal exactly when no command or integration
/// changed.
pub fn declaration_hash(commands: &Commands) -> String {
    crate::ids::canonical_json_hash(&serde_json::json!({
        "steps": commands.steps,
        "integrations": commands.integrations,
    }))
}

/// A change to a repository's commands since the last run that saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandsChanged {
    /// The date (`YYYY-MM-DD`) of the declaration this one replaces.
    pub since: String,
    /// One line per added, removed or changed step or integration.
    pub lines: Vec<String>,
}

impl CommandsChanged {
    /// What `run` prints once on stderr: a header, then one line per change.
    pub fn text(&self) -> String {
        let mut out = format!(
            "relais: commands changed in relais.toml since {}:",
            self.since
        );
        for line in &self.lines {
            out.push_str("\n  ");
            out.push_str(line);
        }
        out
    }

    /// The same, on one line, for the `commands_changed` run event.
    pub fn summary(&self) -> String {
        format!("since {}: {}", self.since, self.lines.join("; "))
    }
}

/// Each step added, removed or changed from `previous` to `now`, and each
/// integration added or removed, as a line a person reads. Empty when the
/// commands are the same.
pub fn commands_diff(previous: &Commands, now: &Commands) -> Vec<String> {
    let mut lines = Vec::new();
    let describe = |step: &Step| {
        format!(
            "{} {} [{}]",
            step.kind.as_str(),
            display_argv(&step.argv),
            escape_display(&step.profile)
        )
    };
    for (step, change) in now.steps.iter().zip(compare(&now.steps, &previous.steps)) {
        match change {
            Change::Same => {}
            Change::New => lines.push(format!("+ added {}", describe(step))),
            Change::Changed => lines.push(format!("~ changed {}", describe(step))),
        }
    }
    // A step of the old list with no counterpart in the new one: `Changed`
    // is the same slot seen from the other side and was said above.
    for (step, change) in previous
        .steps
        .iter()
        .zip(compare(&previous.steps, &now.steps))
    {
        if change == Change::New {
            lines.push(format!("- removed {}", describe(step)));
        }
    }
    for added in now
        .integrations
        .iter()
        .filter(|name| !previous.integrations.contains(name))
    {
        lines.push(format!("+ added integration {}", escape_display(added)));
    }
    for removed in previous
        .integrations
        .iter()
        .filter(|name| !now.integrations.contains(name))
    {
        lines.push(format!("- removed integration {}", escape_display(removed)));
    }
    lines
}

/// The command declaration a repository last ran under, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub hash: String,
    pub commands: Commands,
    /// RFC3339.
    pub at: String,
}

/// Where the declaration each repository last ran under is kept. A
/// caller picks the store whose behaviour it needs: [`Recorder`] (what
/// `relais run` passes) records what it sees, [`Reader`] and
/// [`NoHistory`] (what `plan`, `doctor` and `trust show` pass) record
/// nothing, so looking at a pending change never consumes it. A replay
/// passes [`NoHistory`] and never asks: it neither says nor records one.
pub trait DeclarationStore {
    fn last_seen(&self, repo_key: &str) -> Result<Option<Seen>, String>;
    fn record(&self, repo_key: &str, commands: &Commands) -> Result<(), String>;
}

/// The ledger as a store that records.
pub struct Recorder<'a>(pub &'a Ledger);

/// The ledger as a store that only reads.
pub struct Reader<'a>(pub &'a Ledger);

/// No ledger to read, and nothing to record: `plan` on a machine that has
/// not run relais yet, and a replay, which looks at no pending change.
pub struct NoHistory;

fn seen_in(ledger: &Ledger, repo_key: &str) -> Result<Option<Seen>, String> {
    let Some(stored) = ledger
        .repo_declaration(repo_key)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let commands = serde_json::from_str(&stored.declaration_json).map_err(|e| e.to_string())?;
    Ok(Some(Seen {
        hash: stored.declaration_hash,
        commands,
        at: stored.seen_at,
    }))
}

impl DeclarationStore for Recorder<'_> {
    fn last_seen(&self, repo_key: &str) -> Result<Option<Seen>, String> {
        seen_in(self.0, repo_key)
    }

    fn record(&self, repo_key: &str, commands: &Commands) -> Result<(), String> {
        let json = serde_json::to_string(commands).map_err(|e| e.to_string())?;
        self.0
            .upsert_repo_declaration(repo_key, &declaration_hash(commands), &json)
            .map_err(|e| e.to_string())
    }
}

impl DeclarationStore for Reader<'_> {
    fn last_seen(&self, repo_key: &str) -> Result<Option<Seen>, String> {
        seen_in(self.0, repo_key)
    }

    fn record(&self, _repo_key: &str, _commands: &Commands) -> Result<(), String> {
        Ok(())
    }
}

impl DeclarationStore for NoHistory {
    fn last_seen(&self, _repo_key: &str) -> Result<Option<Seen>, String> {
        Ok(None)
    }

    fn record(&self, _repo_key: &str, _commands: &Commands) -> Result<(), String> {
        Ok(())
    }
}

/// The change from what `repo_key` last ran under to `policy`'s commands,
/// or `None` when they are the same, or when nothing was ever seen (a
/// first sight is recorded, not warned about). A change is handed to the
/// store as seen, so a recording store says it once. A store that cannot
/// be read or written is an `Err` the caller says and goes on from: a
/// warning that cannot be kept never stops a run.
pub fn observe(
    store: &dyn DeclarationStore,
    repo_key: &str,
    policy: &RepoPolicy,
) -> Result<Option<CommandsChanged>, String> {
    let now = commands_of(policy);
    let previous = store.last_seen(repo_key)?;
    let change = match &previous {
        Some(seen) if seen.hash != declaration_hash(&now) => Some(CommandsChanged {
            since: seen.at.chars().take(10).collect(),
            lines: commands_diff(&seen.commands, &now),
        }),
        _ => None,
    };
    if previous.is_none() || change.is_some() {
        store.record(repo_key, &now)?;
    }
    Ok(change)
}

/// How a step compares with the steps the previous grant for the same
/// repository authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// Authorized last time, unchanged.
    Same,
    /// Not authorized last time.
    New,
    /// Its profile and kind had a step at this position, with other argv.
    Changed,
}

/// Each step marked against `previous`. A step whose exact
/// (profile, kind, argv) was granted before is `Same`; one whose
/// (profile, kind) slot at the same index held other argv is `Changed`;
/// anything else is `New`.
pub fn compare(steps: &[Step], previous: &[Step]) -> Vec<Change> {
    let slot = |steps: &[Step], step: &Step| -> Option<usize> {
        steps
            .iter()
            .filter(|other| other.profile == step.profile && other.kind == step.kind)
            .position(|other| std::ptr::eq(other, step))
    };
    steps
        .iter()
        .map(|step| {
            if previous.contains(step) {
                return Change::Same;
            }
            let index = slot(steps, step);
            let held = index.and_then(|index| {
                previous
                    .iter()
                    .filter(|other| other.profile == step.profile && other.kind == step.kind)
                    .nth(index)
            });
            if held.is_some() {
                Change::Changed
            } else {
                Change::New
            }
        })
        .collect()
}

/// Repository-controlled text made safe to put in front of a person: a
/// newline or another control character in a script name must not draw
/// a line of its own (a fake `Allow`), so each is spelled out.
pub fn escape_display(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            // Bidirectional overrides and isolates reorder what is drawn,
            // and the line and paragraph separators break it: either can
            // make one command read as another.
            c if is_reordering(c) => out.push_str(&format!("\\u{{{:04x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn is_reordering(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// An argv as one line a person reads: each argument escaped, and
/// quoted when it holds a space or is empty, so `a b` and `"a b"` look
/// different.
pub fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            let escaped = escape_display(arg);
            if arg.is_empty() || arg.contains(' ') {
                format!("'{escaped}'")
            } else {
                escaped
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What `relais trust show` reports.
#[derive(Debug, Clone, Serialize)]
pub struct Shown {
    pub grant_key: String,
    pub authority_hash: String,
    pub repository: String,
    pub machine_settings: PathBuf,
    /// Whether any grant holds: the repository's, or a legacy exact one.
    pub granted: bool,
    /// The repository grant (`[trust_repos]`), when there is one.
    pub repository_grant: Option<RepositoryGrant>,
    /// [`repo_key`] of the repository: what its grant is recorded under.
    pub repo_key: String,
    pub declaration: Declaration,
    /// A change to the commands that `run` has not yet said; filled by the
    /// caller from a store that records nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_change: Option<CommandsChanged>,
    /// Per step, against the latest grant this machine recorded for the
    /// repository; absent when there is none to compare with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<Change>>,
}

/// Who trusted a repository, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryGrant {
    pub granted_at: String,
    pub reviewed_by: String,
}

pub fn show(
    policy: &RepoPolicy,
    identity: &RepoIdentity,
    machine: &MachineSettings,
    machine_settings: PathBuf,
    previous: Option<&[Step]>,
) -> Shown {
    let authority_hash = policy.authority_hash();
    let key = grant_key(&authority_hash, identity);
    let declaration = declaration(policy);
    let changes = previous.map(|previous| compare(&declaration.steps, previous));
    let key_of_repo = repo_key(identity);
    Shown {
        granted: crate::policy::trust_status(machine, identity, &key)
            != crate::policy::TrustStatus::Missing,
        repository_grant: machine
            .trust_repos
            .get(&key_of_repo)
            .map(|grant| RepositoryGrant {
                granted_at: grant.granted_at.clone(),
                reviewed_by: grant.reviewed_by.clone(),
            }),
        repo_key: key_of_repo,
        pending_change: None,
        grant_key: key,
        authority_hash,
        repository: identity.to_string(),
        machine_settings,
        declaration,
        changes,
    }
}

/// Why [`grant`] wrote nothing.
#[derive(Debug)]
pub enum GrantError {
    /// The machine settings directory or file could not be read, locked
    /// or written.
    Io {
        what: &'static str,
        path: PathBuf,
        cause: std::io::Error,
    },
    /// Another writer held the lock for longer than [`LOCK_WAIT`].
    LockTimeout(PathBuf),
    /// machine.toml as it is on disk is not valid; a grant is not added
    /// to a file nobody can load.
    Invalid(PolicyError),
    /// The edit would not produce a valid file (it cannot, short of a
    /// bug here; checked anyway, because the file is authority).
    Unwritable(String),
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrantError::Io { what, path, cause } => {
                write!(f, "{what} {}: {cause}", path.display())
            }
            GrantError::LockTimeout(path) => write!(
                f,
                "{} is held by another relais for more than {}s; try again",
                path.display(),
                LOCK_WAIT.as_secs()
            ),
            GrantError::Invalid(cause) => write!(f, "machine.toml is not valid: {cause}"),
            GrantError::Unwritable(detail) => {
                write!(f, "machine.toml could not be edited: {detail}")
            }
        }
    }
}

impl std::error::Error for GrantError {}

/// What [`grant`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granted {
    Written,
    /// The key was already there; machine.toml is unchanged.
    AlreadyPresent,
}

/// How long [`grant`] waits for another writer.
pub const LOCK_WAIT: Duration = Duration::from_secs(10);
const LOCK_POLL: Duration = Duration::from_millis(25);

/// machine.toml is never wider than its owner.
const MACHINE_FILE_MODE: u32 = 0o600;

/// Add `[trust."<key>"]` to the machine settings at `path`, creating the
/// file (and its directory) when there is none, through [`edit_machine`].
pub fn grant(path: &Path, key: &str, record: &TrustGrant) -> Result<Granted, GrantError> {
    let written = edit_machine(path, |text, current| {
        if current.trust.contains_key(key) {
            return Ok(None);
        }
        add_grant_to(text, "trust", key, record).map(Some)
    })?;
    Ok(if written {
        Granted::Written
    } else {
        Granted::AlreadyPresent
    })
}

/// Add `[trust_repos."<repo_key>"]` to the machine settings at `path`: the
/// repository is trusted once (ADR-0007). Through [`edit_machine`], so
/// under the lock, atomic, and 0600 for a new file.
pub fn grant_repository(
    path: &Path,
    repo_key: &str,
    record: &TrustGrant,
) -> Result<Granted, GrantError> {
    let written = edit_machine(path, |text, current| {
        if current.trust_repos.contains_key(repo_key) {
            return Ok(None);
        }
        add_grant_to(text, "trust_repos", repo_key, record).map(Some)
    })?;
    Ok(if written {
        Granted::Written
    } else {
        Granted::AlreadyPresent
    })
}

/// The one way relais rewrites machine.toml: `trust grant` and
/// `native router-envelope` both go through it.
///
/// Read, edit, validate and rename happen under an exclusive lock on a
/// sibling `machine.toml.lock`, so two writers at once both land. `edit`
/// gets the file's text (a bare `schema_version` line when there is no
/// file) and its parsed settings, and returns the new text, or `None` to
/// leave the file alone; it edits through `toml_edit`, so a person's
/// comments and ordering survive. The new text is validated as
/// [`MachineSettings`] before it replaces the old one, written
/// atomically, and keeps the old file's mode (0600 for a new one).
/// `true` when the file was written.
pub fn edit_machine(
    path: &Path,
    edit: impl FnOnce(&str, &MachineSettings) -> Result<Option<String>, GrantError>,
) -> Result<bool, GrantError> {
    let io = |what: &'static str, path: &Path| {
        let path = path.to_path_buf();
        move |cause| GrantError::Io { what, path, cause }
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(io("cannot create", dir))?;
    let lock_path = dir.join(format!(
        "{}.lock",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "machine.toml".into())
    ));
    let started = Instant::now();
    let _lock = loop {
        match crate::procs::LockFile::try_acquire(&lock_path)
            .map_err(io("cannot lock", &lock_path))?
        {
            Some(lock) => break lock,
            None if started.elapsed() >= LOCK_WAIT => {
                return Err(GrantError::LockTimeout(lock_path));
            }
            None => std::thread::sleep(LOCK_POLL),
        }
    };

    let (text, mode) = match std::fs::read_to_string(path) {
        Ok(text) => (text, existing_mode(path)),
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => (
            format!("schema_version = {MACHINE_SCHEMA_VERSION}\n"),
            MACHINE_FILE_MODE,
        ),
        Err(cause) => return Err(io("cannot read", path)(cause)),
    };
    let current = MachineSettings::from_toml_str(&text).map_err(GrantError::Invalid)?;
    let Some(edited) = edit(&text, &current)? else {
        return Ok(false);
    };
    MachineSettings::from_toml_str(&edited)
        .map_err(|cause| GrantError::Unwritable(cause.to_string()))?;
    crate::fsutil::write_atomic_with_mode(path, &edited, Some(mode))
        .map_err(io("cannot write", path))?;
    Ok(true)
}

/// The mode an existing file has, so a rewrite keeps it. Off Unix there
/// is none to keep.
fn existing_mode(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            return meta.permissions().mode() & 0o7777;
        }
    }
    let _ = path;
    MACHINE_FILE_MODE
}

/// `text` with one more `[<section>."<key>"]` table, everything else as it
/// was.
fn add_grant_to(
    text: &str,
    section: &str,
    key: &str,
    record: &TrustGrant,
) -> Result<String, GrantError> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| GrantError::Unwritable(e.to_string()))?;
    let trust = doc
        .entry(section)
        .or_insert_with(|| {
            let mut table = toml_edit::Table::new();
            // `[<section>."<key>"]` headers only, no bare `[<section>]` line.
            table.set_implicit(true);
            toml_edit::Item::Table(table)
        })
        .as_table_mut()
        .ok_or_else(|| GrantError::Unwritable(format!("`{section}` is not a table")))?;
    let mut table = toml_edit::Table::new();
    table.insert("granted_at", toml_edit::value(record.granted_at.clone()));
    table.insert("reviewed_by", toml_edit::value(record.reviewed_by.clone()));
    if let Some(repo) = &record.repo {
        table.insert("repo", toml_edit::value(repo.clone()));
    }
    if let Some(note) = &record.note {
        table.insert("note", toml_edit::value(note.clone()));
    }
    trust.insert(key, toml_edit::Item::Table(table));
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> TrustGrant {
        TrustGrant {
            granted_at: "2026-10-07".into(),
            reviewed_by: "a person (in session)".into(),
            note: Some("granted by relais trust grant".into()),
            repo: Some("git@example.com:o/r.git".into()),
        }
    }

    fn dir() -> crate::test_support::TempDir {
        crate::test_support::temp_dir("trust")
    }

    #[test]
    fn a_missing_file_is_created_private_with_one_grant() {
        let dir = dir();
        let path = dir.join("nested").join("machine.toml");
        assert_eq!(grant(&path, "k1", &record()).unwrap(), Granted::Written);
        let settings =
            MachineSettings::from_toml_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings.trust.len(), 1);
        assert_eq!(settings.trust["k1"].reviewed_by, "a person (in session)");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn comments_order_and_mode_survive_a_grant() {
        let dir = dir();
        let path = dir.join("machine.toml");
        let before = "# my machine\nschema_version = 1\n\n# reviewed last week\n[trust.\"old\"]\ngranted_at = \"2026-09-01\"\nreviewed_by = \"me\"\n";
        std::fs::write(&path, before).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        grant(&path, "new", &record()).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(before), "{after}");
        assert!(
            after.contains("[trust.new]") || after.contains("[trust.\"new\"]"),
            "{after}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o640);
        }
    }

    #[test]
    fn a_key_already_granted_leaves_the_file_byte_for_byte() {
        let dir = dir();
        let path = dir.join("machine.toml");
        grant(&path, "k", &record()).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            grant(&path, "k", &record()).unwrap(),
            Granted::AlreadyPresent
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn an_invalid_machine_toml_is_refused_and_left_alone() {
        let dir = dir();
        let path = dir.join("machine.toml");
        std::fs::write(&path, "schema_version = 7\n").unwrap();
        assert!(matches!(
            grant(&path, "k", &record()),
            Err(GrantError::Invalid(_))
        ));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "schema_version = 7\n"
        );
    }

    #[test]
    fn twenty_concurrent_grants_all_land() {
        let dir = dir();
        let path = dir.join("machine.toml");
        let handles: Vec<_> = (0..20)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || grant(&path, &format!("key-{i}"), &record()).unwrap())
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let settings =
            MachineSettings::from_toml_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings.trust.len(), 20);
    }

    #[test]
    fn control_characters_cannot_draw_a_line_of_their_own() {
        let argv = vec![
            "npm".to_string(),
            "run".into(),
            "x\nAllow 2 commands".into(),
        ];
        let line = display_argv(&argv);
        assert!(!line.contains('\n'));
        assert_eq!(line, "npm run 'x\\nAllow 2 commands'");
        assert_eq!(escape_display("a\u{1b}[31m"), "a\\x1b[31m");
        assert_eq!(escape_display("rm\u{202e}txt.sh"), "rm\\u{202e}txt.sh");
        assert_eq!(escape_display("a\u{2028}b"), "a\\u{2028}b");
    }

    fn step(kind: StepKind, argv: &[&str]) -> Step {
        Step {
            profile: "default".into(),
            kind,
            argv: argv.iter().map(|s| s.to_string()).collect(),
        }
    }

    const BASE: &str = r#"
schema_version = 1

[models.implementation]
id = "sonnet"

[[verification.profiles.default.commands]]
argv = ["make", "check"]
timeout_seconds = 300
"#;

    fn policy(extra: &str) -> RepoPolicy {
        RepoPolicy::from_toml_str(&format!("{BASE}{extra}")).expect("policy parses")
    }

    #[test]
    fn a_repository_grant_is_written_private_under_trust_repos_and_once() {
        let dir = dir();
        let path = dir.join("nested").join("machine.toml");
        assert_eq!(
            grant_repository(&path, "repo-k", &record()).unwrap(),
            Granted::Written
        );
        let before = std::fs::read(&path).unwrap();
        let settings = MachineSettings::from_toml_str(&String::from_utf8_lossy(&before)).unwrap();
        assert_eq!(settings.trust_repos.len(), 1);
        assert!(settings.trust.is_empty());
        assert_eq!(
            settings.trust_repos["repo-k"].reviewed_by,
            "a person (in session)"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(
            grant_repository(&path, "repo-k", &record()).unwrap(),
            Granted::AlreadyPresent
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn twenty_concurrent_repository_grants_all_land() {
        let dir = dir();
        let path = dir.join("machine.toml");
        let handles: Vec<_> = (0..20)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    grant_repository(&path, &format!("repo-{i}"), &record()).unwrap()
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let settings =
            MachineSettings::from_toml_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings.trust_repos.len(), 20);
    }

    /// A store in memory that counts what was recorded.
    #[derive(Default)]
    struct Memory {
        seen: std::cell::RefCell<Option<Seen>>,
        records: std::cell::Cell<u32>,
    }

    impl DeclarationStore for Memory {
        fn last_seen(&self, _repo_key: &str) -> Result<Option<Seen>, String> {
            Ok(self.seen.borrow().clone())
        }

        fn record(&self, _repo_key: &str, commands: &Commands) -> Result<(), String> {
            self.records.set(self.records.get() + 1);
            *self.seen.borrow_mut() = Some(Seen {
                hash: declaration_hash(commands),
                commands: commands.clone(),
                at: "2026-10-01T09:00:00Z".into(),
            });
            Ok(())
        }
    }

    #[test]
    fn a_changed_command_warns_once_and_the_next_look_is_silent() {
        let store = Memory::default();
        // First sight is recorded, not warned about.
        assert_eq!(observe(&store, "r", &policy("")).unwrap(), None);
        assert_eq!(observe(&store, "r", &policy("")).unwrap(), None);
        let changed = policy(
            "\n[[verification.profiles.default.commands]]\nargv = [\"cargo\", \"test\"]\ntimeout_seconds = 300\n",
        );
        let change = observe(&store, "r", &changed).unwrap().expect("a change");
        assert_eq!(change.since, "2026-10-01");
        assert_eq!(change.lines, ["+ added check cargo test [default]"]);
        assert!(change
            .text()
            .starts_with("relais: commands changed in relais.toml since 2026-10-01:\n  + added"));
        assert_eq!(observe(&store, "r", &changed).unwrap(), None);
    }

    #[test]
    fn a_removed_or_edited_command_is_named() {
        let store = Memory::default();
        observe(&store, "r", &policy("")).unwrap();
        let edited = RepoPolicy::from_toml_str(&BASE.replace("\"check\"", "\"check\", \"-j4\""))
            .expect("parses");
        let change = observe(&store, "r", &edited).unwrap().expect("a change");
        assert_eq!(change.lines, ["~ changed check make check -j4 [default]"]);
        let none = RepoPolicy::from_toml_str(
            "schema_version = 1\n[models.implementation]\nid = \"sonnet\"\n\
             [verification.profiles.default]\n",
        )
        .expect("parses");
        let change = observe(&store, "r", &none).unwrap().expect("a change");
        assert_eq!(change.lines, ["- removed check make check -j4 [default]"]);
    }

    #[test]
    fn a_recipe_or_model_tier_change_is_not_a_command_change() {
        let store = Memory::default();
        observe(&store, "r", &policy("")).unwrap();
        let recipe = policy(
            "\n[[recipes]]\nname = \"docs\"\nscope_within = [\"docs/**\"]\ntier = \"research\"\n",
        );
        assert_eq!(observe(&store, "r", &recipe).unwrap(), None);
        let other_model =
            RepoPolicy::from_toml_str(&BASE.replace("sonnet", "fable")).expect("parses");
        assert_eq!(observe(&store, "r", &other_model).unwrap(), None);
        assert_eq!(store.records.get(), 1, "only the first sight was recorded");
    }

    #[test]
    fn a_reader_reports_a_change_every_time_and_consumes_nothing() {
        let dir = dir();
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        observe(&Recorder(&ledger), "r", &policy("")).unwrap();
        let changed = policy(
            "\n[[verification.profiles.default.commands]]\nargv = [\"cargo\", \"test\"]\ntimeout_seconds = 300\n",
        );
        for _ in 0..2 {
            assert!(observe(&Reader(&ledger), "r", &changed).unwrap().is_some());
        }
        assert!(observe(&NoHistory, "r", &changed).unwrap().is_none());
        // The recording store says it once.
        assert!(observe(&Recorder(&ledger), "r", &changed)
            .unwrap()
            .is_some());
        assert_eq!(observe(&Recorder(&ledger), "r", &changed).unwrap(), None);
        assert_eq!(observe(&Reader(&ledger), "r", &changed).unwrap(), None);
    }

    #[test]
    fn steps_are_marked_against_the_previous_grant() {
        let previous = vec![step(StepKind::Check, &["make", "check"])];
        let now = vec![
            step(StepKind::Check, &["make", "check"]),
            step(StepKind::Check, &["cargo", "test"]),
        ];
        assert_eq!(compare(&now, &previous), vec![Change::Same, Change::New]);
        let edited = vec![step(StepKind::Check, &["make", "check", "-j4"])];
        assert_eq!(compare(&edited, &previous), vec![Change::Changed]);
    }
}
