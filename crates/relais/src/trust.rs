//! Trust grants issued by a command (SPEC §5): what a grant would
//! authorize, in a form a person can read and a plugin can put to them,
//! and the one writer of machine.toml's `[trust]` tables.
//!
//! A grant is executable authority, so this module never decides to
//! issue one. `relais trust grant` is run by a person in a shell, or by
//! the plugin after the person answered its question; no run, recipe or
//! worker reaches [`grant`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::policy::{
    grant_key, MachineSettings, PolicyError, RepoIdentity, RepoPolicy, TrustGrant,
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
    pub granted: bool,
    pub declaration: Declaration,
    /// Per step, against the latest grant this machine recorded for the
    /// repository; absent when there is none to compare with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<Change>>,
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
    Shown {
        granted: machine.trust.contains_key(&key),
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
            GrantError::Unwritable(detail) => write!(f, "the grant could not be added: {detail}"),
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
/// file (and its directory) when there is none.
///
/// Read, edit, validate and rename happen under an exclusive lock on a
/// sibling `machine.toml.lock`, so two grants at once both land. The
/// edit goes through `toml_edit`, so a person's comments and ordering
/// survive. The new file is validated as [`MachineSettings`] before it
/// replaces the old one, written atomically, and keeps the old file's
/// mode (0600 for a new one).
pub fn grant(path: &Path, key: &str, record: &TrustGrant) -> Result<Granted, GrantError> {
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
    if current.trust.contains_key(key) {
        return Ok(Granted::AlreadyPresent);
    }
    let edited = add_grant(&text, key, record)?;
    MachineSettings::from_toml_str(&edited)
        .map_err(|cause| GrantError::Unwritable(cause.to_string()))?;
    crate::fsutil::write_atomic_with_mode(path, &edited, Some(mode))
        .map_err(io("cannot write", path))?;
    Ok(Granted::Written)
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

/// `text` with one more `[trust."<key>"]` table, everything else as it
/// was.
fn add_grant(text: &str, key: &str, record: &TrustGrant) -> Result<String, GrantError> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| GrantError::Unwritable(e.to_string()))?;
    let trust = doc
        .entry("trust")
        .or_insert_with(|| {
            let mut table = toml_edit::Table::new();
            // `[trust."<key>"]` headers only, no bare `[trust]` line.
            table.set_implicit(true);
            toml_edit::Item::Table(table)
        })
        .as_table_mut()
        .ok_or_else(|| GrantError::Unwritable("`trust` is not a table".into()))?;
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
