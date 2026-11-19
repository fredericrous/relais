//! Programme claims: which session holds which ticket, kept as one file per
//! claim under `<state dir>/claims/<repo-key>/<slug>/<id>.toml`, so two
//! sessions that finish at a boundary minutes apart are not handed the same
//! next ticket.
//!
//! [`held`] is a pure judgement over a claim and a clock. Everything that
//! writes goes through a [`SlugLock`]: the kernel lock on
//! `claims/<repo-key>/<slug>/.lock`, taken for up to [`LOCK_WAIT`], under
//! which the claims are re-read before they are written. A claim is written
//! with `fsutil::write_atomic_with_mode`, so a killed writer never leaves a
//! partial file. Every create, takeover, refresh, steal, release, sweep and
//! expiry appends one line to `claims/log.jsonl`.
//!
//! Selection ([`next`]) takes the programmes as values, already read from
//! git, so it can be tested without a repository.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::model::Mode;
use crate::fsutil::write_atomic_with_mode;
use crate::procs::LockFile;

/// How long a `work` claim holds a ticket after its last refresh (N).
pub const WORK_HOLD_SECS: i64 = 8 * 3600;
/// How long a `handoff` claim holds a ticket after it was made (R): the
/// relay delivers within seconds, and a person may clear by hand later.
pub const HANDOFF_HOLD_SECS: i64 = 2 * 3600;
/// How long a claim file that does not parse holds its ticket.
pub const UNPARSEABLE_HOLD_SECS: i64 = 60;
/// How long a writer waits for a programme's claim lock.
pub const LOCK_WAIT: Duration = Duration::from_secs(2);
const LOCK_POLL: Duration = Duration::from_millis(10);
const CLAIM_FILE_MODE: u32 = 0o600;
/// Under `claims/`: one empty-ish file per session whose end could not delete
/// its claims, named by the session id.
const ENDED_DIR: &str = "ended";

/// What a claim means for its holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The holder is working the ticket.
    Work,
    /// A session that is about to end named the ticket for its successor.
    Handoff,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Handoff => "handoff",
        }
    }
}

/// One claim file's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub session: String,
    pub kind: Kind,
    /// Unix seconds.
    pub claimed_at: i64,
    /// Unix seconds.
    pub refreshed_at: i64,
    /// The session that handed the ticket off, when this claim took one over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The map repository's path, for display.
    #[serde(default)]
    pub repo: String,
}

/// A claim file as found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimRecord {
    Parsed(Claim),
    /// A file that does not parse, with its modification time (Unix seconds).
    Unparsed {
        modified_at: i64,
    },
}

impl ClaimRecord {
    /// The holder's session id; `None` for a file that does not parse.
    pub fn session(&self) -> Option<&str> {
        match self {
            Self::Parsed(claim) => Some(&claim.session),
            Self::Unparsed { .. } => None,
        }
    }

    /// Seconds since the claim was last made or refreshed.
    pub fn age_secs(&self, now: i64) -> u64 {
        let since = match self {
            Self::Parsed(claim) => claim.refreshed_at,
            Self::Unparsed { modified_at } => *modified_at,
        };
        u64::try_from(now.saturating_sub(since)).unwrap_or(0)
    }
}

/// Whether a claim still holds its ticket at `now`: a `work` claim until
/// N after its last refresh, a `handoff` claim until R after it was made,
/// a file that does not parse for a minute. The limit itself still holds.
pub fn held(record: &ClaimRecord, now: i64) -> bool {
    let (since, limit) = match record {
        ClaimRecord::Parsed(Claim {
            kind: Kind::Work,
            refreshed_at,
            ..
        }) => (*refreshed_at, WORK_HOLD_SECS),
        ClaimRecord::Parsed(Claim {
            kind: Kind::Handoff,
            claimed_at,
            ..
        }) => (*claimed_at, HANDOFF_HOLD_SECS),
        ClaimRecord::Unparsed { modified_at } => (*modified_at, UNPARSEABLE_HOLD_SECS),
    };
    now.saturating_sub(since) <= limit
}

/// The current time in Unix seconds; a clock before 1970 reads as 0.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0) // only a clock before 1970 lands here; every claim then reads as just made
}

/// A claim that holds its ticket, as the programme views see it: who, in
/// what capacity, and since when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    /// The holder's session id; empty when the claim file does not parse.
    pub session: String,
    pub kind: Kind,
    /// Unix seconds of the claim's last refresh, or of its file's last write
    /// when it does not parse.
    pub since: i64,
}

impl ClaimRecord {
    fn hold(&self) -> Hold {
        match self {
            Self::Parsed(claim) => Hold {
                session: claim.session.clone(),
                kind: claim.kind,
                since: claim.refreshed_at,
            },
            Self::Unparsed { modified_at } => Hold {
                session: String::new(),
                kind: Kind::Work,
                since: *modified_at,
            },
        }
    }
}

/// The first 12 hex characters of the SHA-256 of a repository's git common
/// directory: the key that names the repository under `claims/`.
pub fn repo_key(common_dir: &Path) -> String {
    let digest = Sha256::digest(common_dir.to_string_lossy().as_bytes());
    digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Which programme a claim belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimKey {
    pub repo_key: String,
    pub slug: String,
}

/// What went wrong with the claim store.
#[derive(Debug)]
pub enum ClaimError {
    /// The programme's `.lock` stayed held for [`LOCK_WAIT`].
    LockTimeout { path: PathBuf },
    Io {
        operation: &'static str,
        path: PathBuf,
        cause: std::io::Error,
    },
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LockTimeout { path } => write!(
                f,
                "the claim lock {} stayed held for {} s",
                path.display(),
                LOCK_WAIT.as_secs()
            ),
            Self::Io {
                operation,
                path,
                cause,
            } => write!(f, "{operation} {}: {cause}", path.display()),
        }
    }
}

impl std::error::Error for ClaimError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LockTimeout { .. } => None,
            Self::Io { cause, .. } => Some(cause),
        }
    }
}

fn io_error(operation: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> ClaimError {
    let path = path.to_path_buf();
    move |cause| ClaimError::Io {
        operation,
        path,
        cause,
    }
}

/// What a claim write did, as the log names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Create,
    Takeover,
    Refresh,
    Steal,
    Release,
    Sweep,
    Expire,
    /// A hook found the programme's lock held and left the claim alone.
    Skipped,
}

impl Event {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Takeover => "takeover",
            Self::Refresh => "refresh",
            Self::Steal => "steal",
            Self::Release => "release",
            Self::Sweep => "sweep",
            Self::Expire => "expire",
            Self::Skipped => "skipped",
        }
    }
}

/// The claims directory of one state directory.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

fn modified_unix(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0) // an unreadable mtime reads as 1970: the file is old, so a broken claim does not hold forever
}

fn read_record(path: &Path) -> Result<Option<ClaimRecord>, ClaimError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        // Not valid UTF-8 is a file that does not parse, not a failure of the store.
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            return Ok(Some(ClaimRecord::Unparsed {
                modified_at: modified_unix(path),
            }));
        }
        Err(e) => return Err(io_error("reading the claim", path)(e)),
    };
    Ok(Some(match toml::from_str::<Claim>(&text) {
        Ok(claim) => ClaimRecord::Parsed(claim),
        Err(_) => ClaimRecord::Unparsed {
            modified_at: modified_unix(path),
        },
    }))
}

fn read_all(slug_dir: &Path) -> Result<BTreeMap<String, ClaimRecord>, ClaimError> {
    let mut records = BTreeMap::new();
    let entries = match std::fs::read_dir(slug_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(records),
        Err(e) => return Err(io_error("listing the claims", slug_dir)(e)),
    };
    for entry in entries {
        let path = entry
            .map_err(io_error("listing the claims", slug_dir))?
            .path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        if let Some(record) = read_record(&path)? {
            records.insert(id, record);
        }
    }
    Ok(records)
}

impl Store {
    /// The store under a state directory.
    pub fn in_state_dir(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("claims"),
        }
    }

    /// The store under the state directory the environment names, or `None`
    /// when no state directory can be found.
    pub fn from_environment() -> Option<Self> {
        crate::paths::state_dir()
            .ok() // no home directory: no claims can have been written, so none are read
            .map(|state| Self::in_state_dir(&state))
    }

    fn slug_dir(&self, key: &ClaimKey) -> PathBuf {
        self.dir.join(&key.repo_key).join(&key.slug)
    }

    /// The ids a programme's claims hold at `now`, read without the lock:
    /// every claim is written whole, so a read sees one claim or the other.
    pub fn held_ids(&self, key: &ClaimKey, now: i64) -> Result<BTreeSet<String>, ClaimError> {
        Ok(read_all(&self.slug_dir(key))?
            .into_iter()
            .filter(|(_, record)| held(record, now))
            .map(|(id, _)| id)
            .collect())
    }

    /// The claims of a programme that hold their tickets at `now`, by ticket
    /// id, read without the lock.
    pub fn holds(&self, key: &ClaimKey, now: i64) -> Result<BTreeMap<String, Hold>, ClaimError> {
        Ok(read_all(&self.slug_dir(key))?
            .into_iter()
            .filter(|(_, record)| held(record, now))
            .map(|(id, record)| (id, record.hold()))
            .collect())
    }

    /// Every claim of a programme, without the lock.
    pub fn records(&self, key: &ClaimKey) -> Result<BTreeMap<String, ClaimRecord>, ClaimError> {
        read_all(&self.slug_dir(key))
    }

    /// Takes the programme's lock, retrying for up to `wait`.
    pub fn lock(&self, key: &ClaimKey, wait: Duration) -> Result<SlugLock<'_>, ClaimError> {
        let dir = self.slug_dir(key);
        std::fs::create_dir_all(&dir).map_err(io_error("creating", &dir))?;
        let path = dir.join(".lock");
        let started = Instant::now();
        loop {
            if let Some(lock) =
                LockFile::try_acquire_private(&path).map_err(io_error("locking", &path))?
            {
                let locked = SlugLock {
                    _lock: lock,
                    store: self,
                    key: key.clone(),
                    dir,
                };
                locked.apply_tombstones(now_unix())?;
                return Ok(locked);
            }
            if started.elapsed() >= wait {
                return Err(ClaimError::LockTimeout { path });
            }
            std::thread::sleep(LOCK_POLL);
        }
    }

    fn log(
        &self,
        event: Event,
        key: &ClaimKey,
        id: &str,
        holder: Option<(&str, Kind)>,
        now: i64,
    ) -> Result<(), ClaimError> {
        let path = self.dir.join("log.jsonl");
        std::fs::create_dir_all(&self.dir).map_err(io_error("creating", &self.dir))?;
        let line = serde_json::json!({
            "ts": now,
            "event": event.as_str(),
            "repo_key": key.repo_key,
            "slug": key.slug,
            "id": id,
            "session": holder.map(|(session, _)| session),
            "kind": holder.map(|(_, kind)| kind.as_str()),
        });
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io_error("opening the claims log", &path))?;
        // One write per line: appended lines from concurrent writers do not interleave.
        file.write_all(format!("{line}\n").as_bytes())
            .map_err(io_error("appending to the claims log", &path))
    }

    /// Moves every `work` claim of `session` to `now`, programme by
    /// programme, each under its own lock; returns how many were moved.
    pub fn refresh_session(&self, session: &str, now: i64) -> Result<usize, ClaimError> {
        let mut refreshed = 0;
        for key in self.keys()? {
            if !self.has_work_claim_in(&key, session)? {
                continue;
            }
            let lock = self.lock(&key, LOCK_WAIT)?;
            refreshed += lock.refresh_work(session, now)?;
        }
        Ok(refreshed)
    }

    /// [`Store::refresh_session`] as a hook does it: each programme's lock
    /// is tried once and never waited for, and a busy one is logged as
    /// `skipped` and left for the next refresh. The session is alive, so a
    /// tombstone left by an earlier end of the same session id is dropped.
    pub fn refresh_session_once(&self, session: &str, now: i64) -> Result<Pass, ClaimError> {
        self.forget_ended(session)?;
        let mut pass = Pass::default();
        for key in self.keys()? {
            if !self.has_work_claim_in(&key, session)? {
                continue;
            }
            match self.lock(&key, Duration::ZERO) {
                Ok(lock) => pass.done += lock.refresh_work(session, now)?,
                Err(ClaimError::LockTimeout { .. }) => {
                    self.log(Event::Skipped, &key, "*", Some((session, Kind::Work)), now)?;
                    pass.skipped += 1;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(pass)
    }

    /// Deletes every `work` claim of an ended session and keeps its
    /// `handoff` claims, trying each programme's lock once. When a lock was
    /// busy the claims there stay, `ended/<session>` is written (without a
    /// lock) and the next locked write in a programme applies it.
    pub fn end_session_once(&self, session: &str, now: i64) -> Result<Pass, ClaimError> {
        let mut pass = Pass::default();
        for key in self.keys()? {
            if !self.has_work_claim_in(&key, session)? {
                continue;
            }
            match self.lock(&key, Duration::ZERO) {
                Ok(lock) => pass.done += lock.remove_work(session, now)?,
                Err(ClaimError::LockTimeout { .. }) => {
                    self.log(Event::Skipped, &key, "*", Some((session, Kind::Work)), now)?;
                    pass.skipped += 1;
                }
                Err(e) => return Err(e),
            }
        }
        if pass.skipped > 0 {
            self.mark_ended(session, now)?;
        }
        Ok(pass)
    }

    fn has_work_claim_in(&self, key: &ClaimKey, session: &str) -> Result<bool, ClaimError> {
        Ok(read_all(&self.slug_dir(key))?
            .values()
            .any(|record| is_work_of(record, session)))
    }

    fn has_work_claim(&self, session: &str) -> Result<bool, ClaimError> {
        for key in self.keys()? {
            if self.has_work_claim_in(&key, session)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn ended_dir(&self) -> PathBuf {
        self.dir.join(ENDED_DIR)
    }

    /// Writes `ended/<session>`: this session's `work` claims are to be
    /// deleted by whichever locked write reaches their programme next.
    pub fn mark_ended(&self, session: &str, now: i64) -> Result<(), ClaimError> {
        if !is_file_name(session) {
            return Ok(());
        }
        let dir = self.ended_dir();
        std::fs::create_dir_all(&dir).map_err(io_error("creating", &dir))?;
        let path = dir.join(session);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => file
                .write_all(now.to_string().as_bytes())
                .map_err(io_error("writing the tombstone", &path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(io_error("creating the tombstone", &path)(e)),
        }
    }

    /// Deletes `ended/<session>`; there being none is not a failure.
    pub fn forget_ended(&self, session: &str) -> Result<(), ClaimError> {
        if !is_file_name(session) {
            return Ok(());
        }
        let path = self.ended_dir().join(session);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error("deleting the tombstone", &path)(e)),
        }
    }

    fn ended_sessions(&self) -> Result<Vec<String>, ClaimError> {
        let dir = self.ended_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_error("listing the tombstones", &dir)(e)),
        };
        let mut sessions = Vec::new();
        for entry in entries {
            let name = entry
                .map_err(io_error("listing the tombstones", &dir))?
                .file_name()
                .to_string_lossy()
                .into_owned();
            if is_file_name(&name) {
                sessions.push(name);
            }
        }
        sessions.sort();
        Ok(sessions)
    }

    fn keys(&self) -> Result<Vec<ClaimKey>, ClaimError> {
        let mut keys = Vec::new();
        let repos = match std::fs::read_dir(&self.dir) {
            Ok(repos) => repos,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(keys),
            Err(e) => return Err(io_error("listing", &self.dir)(e)),
        };
        for repo in repos {
            let repo = repo.map_err(io_error("listing", &self.dir))?.path();
            if !repo.is_dir() {
                continue;
            }
            let Some(repo_key) = repo.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if repo_key == ENDED_DIR {
                continue;
            }
            for slug in std::fs::read_dir(&repo).map_err(io_error("listing", &repo))? {
                let slug = slug.map_err(io_error("listing", &repo))?.path();
                if let (true, Some(name)) = (slug.is_dir(), slug.file_name()) {
                    keys.push(ClaimKey {
                        repo_key: repo_key.clone(),
                        slug: name.to_string_lossy().into_owned(),
                    });
                }
            }
        }
        keys.sort_by(|a, b| (&a.repo_key, &a.slug).cmp(&(&b.repo_key, &b.slug)));
        Ok(keys)
    }
}

/// What a hook's pass over a session's claims came to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pass {
    /// Claims refreshed or deleted.
    pub done: usize,
    /// Programmes whose lock was busy and were left alone.
    pub skipped: usize,
}

fn is_work_of(record: &ClaimRecord, session: &str) -> bool {
    matches!(record, ClaimRecord::Parsed(c) if c.session == session && c.kind == Kind::Work)
}

/// Whether a session id is safe to use as one file name.
fn is_file_name(session: &str) -> bool {
    !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A programme's claim lock, held: the only way to write a claim.
#[derive(Debug)]
pub struct SlugLock<'a> {
    _lock: LockFile,
    store: &'a Store,
    key: ClaimKey,
    dir: PathBuf,
}

/// What asking for a ticket as `work` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Taken {
    /// The claim was written; the event says how.
    Claimed(Event),
    /// Another live session holds the ticket as `work`.
    Refused {
        /// The holder's session id; empty when the claim file does not parse.
        holder: String,
        age_secs: u64,
    },
}

/// What releasing a ticket came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Released {
    Released,
    /// There was no claim to release.
    Absent,
    /// Another session holds it; nothing was deleted.
    NotHolder {
        holder: String,
    },
}

impl SlugLock<'_> {
    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.toml"))
    }

    fn write(&self, id: &str, claim: &Claim) -> Result<(), ClaimError> {
        let path = self.path(id);
        let text = toml::to_string(claim).map_err(|e| ClaimError::Io {
            operation: "encoding the claim",
            path: path.clone(),
            cause: std::io::Error::other(e),
        })?;
        write_atomic_with_mode(&path, &text, Some(CLAIM_FILE_MODE))
            .map_err(io_error("writing the claim", &path))
    }

    fn remove(&self, id: &str) -> Result<(), ClaimError> {
        let path = self.path(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error("deleting the claim", &path)(e)),
        }
    }

    fn log(
        &self,
        event: Event,
        id: &str,
        holder: Option<(&str, Kind)>,
        now: i64,
    ) -> Result<(), ClaimError> {
        self.store.log(event, &self.key, id, holder, now)
    }

    fn log_record(
        &self,
        event: Event,
        id: &str,
        record: &ClaimRecord,
        now: i64,
    ) -> Result<(), ClaimError> {
        match record {
            ClaimRecord::Parsed(claim) => {
                self.log(event, id, Some((&claim.session, claim.kind)), now)
            }
            ClaimRecord::Unparsed { .. } => self.log(event, id, None, now),
        }
    }

    /// Moves this programme's `work` claims of `session` to `now`.
    fn refresh_work(&self, session: &str, now: i64) -> Result<usize, ClaimError> {
        let mut refreshed = 0;
        for (id, record) in read_all(&self.dir)? {
            if let ClaimRecord::Parsed(claim) = record {
                if claim.session == session && claim.kind == Kind::Work {
                    self.write(
                        &id,
                        &Claim {
                            refreshed_at: now,
                            ..claim
                        },
                    )?;
                    self.log(Event::Refresh, &id, Some((session, Kind::Work)), now)?;
                    refreshed += 1;
                }
            }
        }
        Ok(refreshed)
    }

    /// Deletes this programme's `work` claims of `session`; its `handoff`
    /// claims stay.
    fn remove_work(&self, session: &str, now: i64) -> Result<usize, ClaimError> {
        let mut removed = 0;
        for (id, record) in read_all(&self.dir)? {
            if is_work_of(&record, session) {
                self.remove(&id)?;
                self.log_record(Event::Release, &id, &record, now)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Applies `ended/<session>` for every ended session: its `work` claims
    /// here go, and the tombstone with them once no programme holds one.
    fn apply_tombstones(&self, now: i64) -> Result<(), ClaimError> {
        for session in self.store.ended_sessions()? {
            self.remove_work(&session, now)?;
            if !self.store.has_work_claim(&session)? {
                self.store.forget_ended(&session)?;
            }
        }
        Ok(())
    }

    /// Deletes the claims of closed tickets and the ones that no longer hold.
    pub fn sweep(&self, closed: &BTreeSet<String>, now: i64) -> Result<(), ClaimError> {
        for (id, record) in read_all(&self.dir)? {
            if closed.contains(&id) {
                self.remove(&id)?;
                self.log_record(Event::Sweep, &id, &record, now)?;
            } else if !held(&record, now) {
                self.remove(&id)?;
                self.log_record(Event::Expire, &id, &record, now)?;
            }
        }
        Ok(())
    }

    /// Takes `id` for `session` as `work`: a new claim, a refresh of its own,
    /// a takeover of a `handoff`, or (with `steal`) another session's `work`.
    pub fn claim_work(
        &self,
        id: &str,
        session: &str,
        repo_display: &str,
        steal: bool,
        now: i64,
    ) -> Result<Taken, ClaimError> {
        let existing = read_all(&self.dir)?.remove(id);
        let fresh = |from: Option<String>, claimed_at: i64| Claim {
            session: session.to_string(),
            kind: Kind::Work,
            claimed_at,
            refreshed_at: now,
            from,
            repo: repo_display.to_string(),
        };
        let (claim, event) = match &existing {
            None => (fresh(None, now), Event::Create),
            Some(record) if !held(record, now) => {
                self.log_record(Event::Expire, id, record, now)?;
                (fresh(None, now), Event::Create)
            }
            Some(ClaimRecord::Parsed(held_claim)) if held_claim.session == session => {
                let event = match held_claim.kind {
                    Kind::Work => Event::Refresh,
                    Kind::Handoff => Event::Takeover,
                };
                (fresh(held_claim.from.clone(), held_claim.claimed_at), event)
            }
            Some(ClaimRecord::Parsed(Claim {
                kind: Kind::Handoff,
                session: handed_by,
                ..
            })) => (fresh(Some(handed_by.clone()), now), Event::Takeover),
            Some(record) if !steal => {
                return Ok(Taken::Refused {
                    holder: record.session().unwrap_or_default().to_string(),
                    age_secs: record.age_secs(now),
                });
            }
            Some(record) => (
                fresh(record.session().map(str::to_string), now),
                Event::Steal,
            ),
        };
        self.write(id, &claim)?;
        self.log(event, id, Some((session, Kind::Work)), now)?;
        Ok(Taken::Claimed(event))
    }

    /// Deletes `id`'s claim when `session` holds it.
    pub fn release(&self, id: &str, session: &str, now: i64) -> Result<Released, ClaimError> {
        match read_all(&self.dir)?.remove(id) {
            None => Ok(Released::Absent),
            Some(ClaimRecord::Parsed(claim)) if claim.session == session => {
                self.remove(id)?;
                self.log(Event::Release, id, Some((session, claim.kind)), now)?;
                Ok(Released::Released)
            }
            Some(record) => Ok(Released::NotHolder {
                holder: record.session().unwrap_or_default().to_string(),
            }),
        }
    }

    /// Writes a `handoff` claim on `id` unless it is held; whether it was written.
    pub fn reserve_handoff(
        &self,
        id: &str,
        session: &str,
        repo_display: &str,
        now: i64,
    ) -> Result<bool, ClaimError> {
        if let Some(record) = read_all(&self.dir)?.remove(id) {
            if held(&record, now) {
                return Ok(false);
            }
            self.log_record(Event::Expire, id, &record, now)?;
        }
        let claim = Claim {
            session: session.to_string(),
            kind: Kind::Handoff,
            claimed_at: now,
            refreshed_at: now,
            from: None,
            repo: repo_display.to_string(),
        };
        self.write(id, &claim)?;
        self.log(Event::Create, id, Some((session, Kind::Handoff)), now)?;
        Ok(true)
    }

    /// Writes a `handoff` claim on the first of `candidates`, only when the
    /// programme is empty: no open ticket held by a claim, none by a branch.
    /// The check and the write are one step under the lock, so two callers
    /// entering the same empty programme cannot both reserve a ticket there.
    pub fn reserve_in_empty_programme(
        &self,
        candidates: &[&str],
        open: &BTreeSet<String>,
        branch_held: &BTreeSet<String>,
        session: &str,
        repo_display: &str,
        now: i64,
    ) -> Result<Option<String>, ClaimError> {
        let Some(first) = candidates.first() else {
            return Ok(None);
        };
        let claimed = read_all(&self.dir)?
            .into_iter()
            .any(|(id, record)| open.contains(&id) && held(&record, now));
        if claimed || !branch_held.is_empty() {
            return Ok(None);
        }
        Ok(self
            .reserve_handoff(first, session, repo_display, now)?
            .then(|| (*first).to_string()))
    }
}

/// A ready ticket, as selection sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyTicket {
    pub id: String,
    pub name: String,
    pub due: Option<NaiveDate>,
    pub mode: Mode,
}

/// One programme as selection sees it: read from git by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgrammeState {
    /// The map repository's directory name.
    pub repo: String,
    /// The map repository's path, recorded in a claim for display.
    pub repo_path: String,
    pub slug: String,
    pub key: ClaimKey,
    /// Open tickets with every blocker closed and no branch, in frontier order.
    pub ready: Vec<ReadyTicket>,
    pub open: BTreeSet<String>,
    pub closed: BTreeSet<String>,
    /// Tickets a `prog/<slug>/<id>` branch works.
    pub branch_held: BTreeSet<String>,
}

/// The ticket `next` chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub repo: String,
    pub slug: String,
    pub id: String,
    pub name: String,
}

/// What `next` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Pick(Pick),
    /// Nothing is eligible; the soonest day an otherwise-ready, unheld,
    /// `afk` ticket falls due, if there is one.
    Parked {
        next_due: Option<NaiveDate>,
    },
}

impl Next {
    /// The line `next` prints when it parks.
    pub fn parked_line(next_due: Option<NaiveDate>) -> String {
        match next_due {
            Some(date) => format!("parked: next due {date}"),
            None => "parked: nothing due".to_string(),
        }
    }
}

/// Whether the person is not needed and the ticket is not scheduled later.
fn eligible(ticket: &ReadyTicket, today: NaiveDate) -> bool {
    ticket.mode == Mode::Afk && ticket.due.is_none_or(|due| due <= today)
}

/// A programme's unheld ready tickets, `afk` only, in frontier order.
fn unheld_afk<'a>(state: &'a ProgrammeState, held_ids: &BTreeSet<String>) -> Vec<&'a ReadyTicket> {
    state
        .ready
        .iter()
        .filter(|ticket| ticket.mode == Mode::Afk && !held_ids.contains(&ticket.id))
        .collect()
}

/// Picks the next ticket for a session at a boundary.
///
/// 1. The first unheld, eligible ready ticket of `from`.
/// 2. Otherwise the first eligible ready ticket of a programme in which no
///    ticket is held, programmes ordered like the root summary (soonest
///    due, then most ready, then name).
/// 3. Otherwise [`Next::Parked`].
///
/// With `reserve_for` a session id, the choice is written as a `handoff`
/// claim; a lost race moves on to the following candidate. Step 1 releases
/// `from`'s lock before step 2 takes a destination's, and a destination
/// lock not acquired within [`LOCK_WAIT`] counts as occupied.
pub fn next(
    store: &Store,
    from: Option<&ProgrammeState>,
    others: &[ProgrammeState],
    today: NaiveDate,
    now: i64,
    reserve_for: Option<&str>,
) -> Result<Next, ClaimError> {
    let mut wake: Option<NaiveDate> = None;
    let mut note_wake = |tickets: &[&ReadyTicket]| {
        for ticket in tickets {
            if let Some(due) = ticket.due.filter(|due| *due > today) {
                wake = Some(wake.map_or(due, |soonest| soonest.min(due)));
            }
        }
    };

    if let Some(state) = from {
        let held_ids = store.held_ids(&state.key, now)?;
        let unheld = unheld_afk(state, &held_ids);
        note_wake(&unheld);
        for ticket in unheld.iter().filter(|t| eligible(t, today)) {
            let pick = || {
                Next::Pick(Pick {
                    repo: state.repo.clone(),
                    slug: state.slug.clone(),
                    id: ticket.id.clone(),
                    name: ticket.name.clone(),
                })
            };
            let Some(session) = reserve_for else {
                return Ok(pick());
            };
            let lock = store.lock(&state.key, LOCK_WAIT)?;
            lock.sweep(&state.closed, now)?;
            if lock.reserve_handoff(&ticket.id, session, &state.repo_path, now)? {
                return Ok(pick());
            }
        }
    }

    struct Destination<'a> {
        state: &'a ProgrammeState,
        eligible: Vec<&'a ReadyTicket>,
    }
    let mut destinations = Vec::new();
    for state in others {
        let held_ids: BTreeSet<String> = store
            .held_ids(&state.key, now)?
            .into_iter()
            .filter(|id| state.open.contains(id))
            .collect();
        let unheld = unheld_afk(state, &held_ids);
        note_wake(&unheld);
        let is_empty = held_ids.is_empty() && state.branch_held.is_empty();
        let eligible_tickets: Vec<&ReadyTicket> = unheld
            .into_iter()
            .filter(|ticket| eligible(ticket, today))
            .collect();
        if is_empty && !eligible_tickets.is_empty() {
            destinations.push(Destination {
                state,
                eligible: eligible_tickets,
            });
        }
    }
    destinations.sort_by_key(|destination| {
        let soonest = destination.eligible.iter().filter_map(|t| t.due).min();
        (
            soonest.is_none(),
            soonest,
            std::cmp::Reverse(destination.eligible.len()),
            destination.state.repo.clone(),
            destination.state.slug.clone(),
        )
    });
    for Destination { state, eligible } in &destinations {
        let first = eligible[0];
        let pick = |id: &str, name: &str| {
            Next::Pick(Pick {
                repo: state.repo.clone(),
                slug: state.slug.clone(),
                id: id.to_string(),
                name: name.to_string(),
            })
        };
        let Some(session) = reserve_for else {
            return Ok(pick(&first.id, &first.name));
        };
        let lock = match store.lock(&state.key, LOCK_WAIT) {
            Ok(lock) => lock,
            Err(ClaimError::LockTimeout { .. }) => continue,
            Err(e) => return Err(e),
        };
        lock.sweep(&state.closed, now)?;
        let ids: Vec<&str> = eligible.iter().map(|t| t.id.as_str()).collect();
        if let Some(id) = lock.reserve_in_empty_programme(
            &ids,
            &state.open,
            &state.branch_held,
            session,
            &state.repo_path,
            now,
        )? {
            let name = eligible
                .iter()
                .find(|t| t.id == id)
                .map_or(first.name.as_str(), |t| t.name.as_str());
            return Ok(pick(&id, name));
        }
    }
    Ok(Next::Parked { next_due: wake })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::short_temp_dir;

    const NOW: i64 = 1_800_000_000;

    fn claim(kind: Kind, claimed_ago: i64, refreshed_ago: i64) -> ClaimRecord {
        ClaimRecord::Parsed(Claim {
            session: "s1".into(),
            kind,
            claimed_at: NOW - claimed_ago,
            refreshed_at: NOW - refreshed_ago,
            from: None,
            repo: String::new(),
        })
    }

    fn day(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, day).expect("a date")
    }

    #[test]
    fn a_work_claim_holds_for_n_after_its_refresh_and_not_a_second_longer() {
        assert!(held(&claim(Kind::Work, 99_999, WORK_HOLD_SECS), NOW));
        assert!(!held(&claim(Kind::Work, 0, WORK_HOLD_SECS + 1), NOW));
        // Only the refresh counts for work: an old claim, freshly refreshed, holds.
        assert!(held(&claim(Kind::Work, WORK_HOLD_SECS * 3, 5), NOW));
    }

    #[test]
    fn a_handoff_claim_holds_for_r_after_it_was_made_and_not_a_second_longer() {
        assert!(held(&claim(Kind::Handoff, HANDOFF_HOLD_SECS, 0), NOW));
        assert!(!held(&claim(Kind::Handoff, HANDOFF_HOLD_SECS + 1, 0), NOW));
        // A handoff is not extended by a refresh.
        assert!(!held(&claim(Kind::Handoff, HANDOFF_HOLD_SECS + 1, 1), NOW));
    }

    #[test]
    fn a_claim_that_does_not_parse_holds_for_a_minute() {
        let unparsed = |age: i64| ClaimRecord::Unparsed {
            modified_at: NOW - age,
        };
        assert!(held(&unparsed(UNPARSEABLE_HOLD_SECS - 1), NOW));
        assert!(held(&unparsed(UNPARSEABLE_HOLD_SECS), NOW));
        assert!(!held(&unparsed(UNPARSEABLE_HOLD_SECS + 1), NOW));
    }

    #[test]
    fn the_repo_key_is_twelve_hex_characters_of_the_sha256() {
        // sha256("abc") = ba7816bf8f01cfea414140de5dae2223…
        assert_eq!(repo_key(Path::new("abc")), "ba7816bf8f01");
    }

    struct Fixture {
        scratch: crate::test_support::TempDir,
        store: Store,
    }

    fn fixture(tag: &str) -> Fixture {
        let scratch = short_temp_dir(&format!("claim-{tag}"));
        let store = Store::in_state_dir(&scratch);
        Fixture { scratch, store }
    }

    fn ready(id: &str, due: Option<NaiveDate>, mode: Mode) -> ReadyTicket {
        ReadyTicket {
            id: id.into(),
            name: format!("name of {id}"),
            due,
            mode,
        }
    }

    fn programme(repo: &str, slug: &str, ready: Vec<ReadyTicket>) -> ProgrammeState {
        let open = ready.iter().map(|t| t.id.clone()).collect();
        ProgrammeState {
            repo: repo.into(),
            repo_path: format!("/r/{repo}"),
            slug: slug.into(),
            key: ClaimKey {
                repo_key: repo_key(Path::new(repo)),
                slug: slug.into(),
            },
            ready,
            open,
            closed: BTreeSet::new(),
            branch_held: BTreeSet::new(),
        }
    }

    fn picked(next: &Next) -> String {
        match next {
            Next::Pick(p) => format!("{}:{}:{}", p.repo, p.slug, p.id),
            Next::Parked { .. } => "parked".to_string(),
        }
    }

    fn afk(id: &str) -> ReadyTicket {
        ready(id, None, Mode::Afk)
    }

    #[test]
    fn the_from_programme_comes_first_then_a_programme_with_nothing_held() {
        let f = fixture("order");
        let from = programme("a", "own", vec![afk("t1"), afk("t2")]);
        let other = programme("b", "far", vec![afk("x1")]);
        let chosen = |from: Option<&ProgrammeState>| {
            picked(
                &next(
                    &f.store,
                    from,
                    std::slice::from_ref(&other),
                    day(19),
                    NOW,
                    None,
                )
                .unwrap(),
            )
        };
        assert_eq!(chosen(Some(&from)), "a:own:t1");
        assert_eq!(chosen(None), "b:far:x1");

        // A held first ticket is passed over inside the from programme.
        let lock = f.store.lock(&from.key, LOCK_WAIT).unwrap();
        assert!(lock.reserve_handoff("t1", "s9", "/r/a", NOW).unwrap());
        drop(lock);
        assert_eq!(chosen(Some(&from)), "a:own:t2");
    }

    #[test]
    fn a_programme_with_a_held_ticket_is_not_entered_and_the_rest_are_ordered_like_the_summary() {
        let f = fixture("dest");
        let busy = programme("a", "busy", vec![afk("t1"), afk("t2")]);
        f.store
            .lock(&busy.key, LOCK_WAIT)
            .unwrap()
            .reserve_handoff("t1", "s9", "/r/a", NOW)
            .unwrap();
        let mut branch = programme("b", "branch", vec![afk("t1"), afk("t2")]);
        branch.branch_held.insert("t9".into());
        let few = programme("c", "few", vec![afk("t1")]);
        let many = programme("d", "many", vec![afk("t1"), afk("t2")]);
        let dated = programme("e", "dated", vec![ready("t1", Some(day(10)), Mode::Afk)]);
        let others = [busy, branch, few, many, dated];
        let chosen = |others: &[ProgrammeState]| {
            picked(&next(&f.store, None, others, day(19), NOW, None).unwrap())
        };
        assert_eq!(chosen(&others), "e:dated:t1", "soonest due first");
        assert_eq!(chosen(&others[..4]), "d:many:t1", "then most ready");
        assert_eq!(chosen(&others[..3]), "c:few:t1", "busy and branch skipped");
    }

    #[test]
    fn a_future_due_or_a_ticket_for_the_person_is_never_selected_and_the_park_names_the_date() {
        let f = fixture("park");
        let from = programme(
            "a",
            "own",
            vec![
                ready("later", Some(day(24)), Mode::Afk),
                ready("soon", Some(day(21)), Mode::Afk),
                ready("person", None, Mode::Hitl),
            ],
        );
        let run = |today| next(&f.store, Some(&from), &[], today, NOW, None).unwrap();
        assert_eq!(
            run(day(19)),
            Next::Parked {
                next_due: Some(day(21))
            }
        );
        assert_eq!(picked(&run(day(21))), "a:own:soon");
        let only_person = programme("b", "p", vec![ready("p", None, Mode::Hitl)]);
        assert_eq!(
            next(&f.store, None, &[only_person], day(19), NOW, None).unwrap(),
            Next::Parked { next_due: None }
        );
        assert_eq!(Next::parked_line(None), "parked: nothing due");
        assert_eq!(
            Next::parked_line(Some(day(21))),
            "parked: next due 2026-11-21"
        );
    }

    #[test]
    fn a_reserving_next_makes_a_handoff_claim_so_the_second_caller_takes_the_next_ticket() {
        let f = fixture("reserve");
        let from = programme("a", "own", vec![afk("t1"), afk("t2")]);
        let take = |session: &str| {
            picked(&next(&f.store, Some(&from), &[], day(19), NOW, Some(session)).unwrap())
        };
        assert_eq!(take("s1"), "a:own:t1");
        assert_eq!(take("s2"), "a:own:t2");
        assert_eq!(take("s3"), "parked");
        let records = f.store.records(&from.key).unwrap();
        assert!(
            matches!(&records["t1"], ClaimRecord::Parsed(c) if c.kind == Kind::Handoff && c.session == "s1")
        );
        let log =
            std::fs::read_to_string(f.scratch.to_path_buf().join("claims/log.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 2);
        assert!(log.contains("\"event\":\"create\""));
    }

    #[test]
    fn two_callers_entering_one_empty_programme_reserve_at_most_one_ticket_there() {
        let f = fixture("empty");
        let destination = programme("z", "empty", vec![afk("t1"), afk("t2")]);
        let one = next(
            &f.store,
            None,
            std::slice::from_ref(&destination),
            day(19),
            NOW,
            Some("s1"),
        )
        .unwrap();
        let two = next(
            &f.store,
            None,
            std::slice::from_ref(&destination),
            day(19),
            NOW,
            Some("s2"),
        )
        .unwrap();
        assert_eq!(picked(&one), "z:empty:t1");
        assert_eq!(picked(&two), "parked");
    }

    #[test]
    fn claiming_takes_over_a_handoff_refuses_a_live_work_claim_and_steals_on_request() {
        let f = fixture("claim");
        let key = ClaimKey {
            repo_key: "k".into(),
            slug: "p".into(),
        };
        let lock = f.store.lock(&key, LOCK_WAIT).unwrap();
        assert!(lock.reserve_handoff("t1", "old", "/r", NOW).unwrap());
        assert_eq!(
            lock.claim_work("t1", "a", "/r", false, NOW).unwrap(),
            Taken::Claimed(Event::Takeover)
        );
        assert_eq!(
            lock.claim_work("t1", "b", "/r", false, NOW + 5).unwrap(),
            Taken::Refused {
                holder: "a".into(),
                age_secs: 5
            }
        );
        assert_eq!(
            lock.claim_work("t1", "a", "/r", false, NOW + 9).unwrap(),
            Taken::Claimed(Event::Refresh)
        );
        assert_eq!(
            lock.claim_work("t1", "b", "/r", true, NOW + 9).unwrap(),
            Taken::Claimed(Event::Steal)
        );
        assert_eq!(
            lock.release("t1", "a", NOW + 9).unwrap(),
            Released::NotHolder { holder: "b".into() }
        );
        assert_eq!(
            lock.release("t1", "b", NOW + 9).unwrap(),
            Released::Released
        );
        assert_eq!(lock.release("t1", "b", NOW + 9).unwrap(), Released::Absent);
    }

    #[test]
    fn an_expired_claim_is_replaced_and_a_closed_tickets_claim_is_swept() {
        let f = fixture("sweep");
        let key = ClaimKey {
            repo_key: "k".into(),
            slug: "p".into(),
        };
        let lock = f.store.lock(&key, LOCK_WAIT).unwrap();
        lock.claim_work("t1", "a", "/r", false, NOW).unwrap();
        lock.claim_work("t2", "a", "/r", false, NOW).unwrap();
        let closed: BTreeSet<String> = ["t2".to_string()].into();
        let later = NOW + WORK_HOLD_SECS + 1;
        lock.sweep(&closed, NOW).unwrap();
        assert_eq!(f.store.records(&key).unwrap().len(), 1);
        assert_eq!(
            lock.claim_work("t1", "b", "/r", false, later).unwrap(),
            Taken::Claimed(Event::Create)
        );
    }

    #[test]
    fn a_session_refresh_moves_only_its_own_work_claims_past_n() {
        let f = fixture("refresh");
        let key = ClaimKey {
            repo_key: "k".into(),
            slug: "p".into(),
        };
        {
            let lock = f.store.lock(&key, LOCK_WAIT).unwrap();
            lock.claim_work("t1", "a", "/r", false, NOW).unwrap();
            lock.claim_work("t2", "b", "/r", false, NOW).unwrap();
        }
        let later = NOW + WORK_HOLD_SECS - 1;
        assert_eq!(f.store.refresh_session("a", later).unwrap(), 1);
        let after = later + 2;
        assert_eq!(
            f.store.held_ids(&key, after).unwrap(),
            ["t1".to_string()].into_iter().collect()
        );
    }
}
