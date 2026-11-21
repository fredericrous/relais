//! The claim store: one file per claim under
//! `<state dir>/claims/<repo-key>/<slug>/<id>.toml`.
//!
//! Everything that writes goes through a [`SlugLock`]: the kernel lock on
//! `claims/<repo-key>/<slug>/.lock`, taken for up to the caller's wait, under
//! which the claims are re-read before they are written. A claim is written
//! with `fsutil::write_atomic_with_mode`, so a killed writer never leaves a
//! partial file. Every create, takeover, refresh, steal, release, sweep and
//! expiry appends one line to `claims/log.jsonl`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::rules::{
    held, is_work_of, Claim, ClaimKey, ClaimRecord, Contest, Hold, Holder, Kind, SessionClaim,
    SessionClaims,
};
use crate::fsutil::write_atomic_with_mode;
use crate::procs::LockFile;

/// How long a writer waits for a programme's claim lock.
pub const LOCK_WAIT: Duration = Duration::from_secs(2);
const LOCK_POLL: Duration = Duration::from_millis(10);
/// How long one refresh or end pass waits, in total, for programme locks.
/// A lock reads as held for a moment after its holder let go when a
/// concurrent fork copied the descriptor and has not exec'd yet (see
/// [`crate::procs::LockFile`]); a lock still held after this is skipped.
/// It is the only wait a SessionStart or SessionEnd hook spends on a busy
/// lock, so it is kept well under 100 ms (checked at compile time below);
/// the pass's file writes come on top and are not bounded by it. The hook
/// itself is killed only at `install::settings::OTHER_HANDLER_TIMEOUT`.
/// The `hook::respond` tests `…_with_the_lock_held_elsewhere_…` check that
/// a busy lock costs the grace and not [`LOCK_WAIT`].
const ONCE_GRACE: Duration = Duration::from_millis(30);
const _: () = assert!(
    ONCE_GRACE.as_millis() < 100,
    "a hook's lock wait stays well under 100 ms"
);
const CLAIM_FILE_MODE: u32 = 0o600;
/// Under `claims/`: one empty-ish file per session whose end could not delete
/// its claims, named by the session id.
const ENDED_DIR: &str = "ended";

/// The current time in Unix seconds; a clock before 1970 reads as 0.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0) // only a clock before 1970 lands here; every claim then reads as just made
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
    /// A claim could not be written as TOML.
    Encode {
        path: PathBuf,
        source: toml::ser::Error,
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
            Self::Encode { path, source } => {
                write!(f, "encoding the claim {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ClaimError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LockTimeout { .. } => None,
            Self::Io { cause, .. } => Some(cause),
            Self::Encode { source, .. } => Some(source),
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

    /// Every claim `session` wrote, across all programmes, read without the
    /// lock and without sweeping or changing anything: those holding at
    /// `now`, and the kinds of those that no longer do. A `work` claim of a
    /// session that has an `ended/<session>` tombstone is ignored, as the
    /// next locked write would delete it.
    pub fn claims_of(&self, session: &str, now: i64) -> Result<SessionClaims, ClaimError> {
        let ended = self.ended_sessions()?.iter().any(|name| name == session);
        let mut found = SessionClaims::default();
        for key in self.keys()? {
            for (id, record) in read_all(&self.slug_dir(&key))? {
                let ClaimRecord::Parsed(claim) = &record else {
                    continue;
                };
                if claim.session != session || (ended && claim.kind == Kind::Work) {
                    continue;
                }
                if held(&record, now) {
                    found.live.push(SessionClaim {
                        key: key.clone(),
                        id,
                        claim: claim.clone(),
                    });
                } else {
                    found.expired.push(claim.kind);
                }
            }
        }
        Ok(found)
    }

    /// Takes the programme's lock, retrying for up to `wait`, and applies the
    /// tombstones of ended sessions as of `now`. A tombstone that cannot be
    /// applied is reported on stderr and does not keep the lock from the caller.
    pub fn lock(
        &self,
        key: &ClaimKey,
        wait: Duration,
        now: i64,
    ) -> Result<SlugLock<'_>, ClaimError> {
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
                if let Err(error) = locked.apply_tombstones(now) {
                    eprintln!(
                        "relais: claims {}/{}: applying the ended sessions failed: {error}",
                        key.repo_key, key.slug
                    );
                }
                return Ok(locked);
            }
            let elapsed = started.elapsed();
            if elapsed >= wait {
                return Err(ClaimError::LockTimeout { path });
            }
            // The last sleep stops at the deadline, so `wait` bounds the wait.
            std::thread::sleep(LOCK_POLL.min(wait - elapsed));
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

    /// Moves every `work` claim of `session` to `now`, waiting for the
    /// programme locks no longer than [`ONCE_GRACE`] for the whole pass: one
    /// still busy is logged as `skipped` and left for the next refresh. A
    /// programme that fails is recorded in [`Pass::failures`] and the others
    /// are still refreshed.
    /// The session is alive, so a tombstone left by an earlier end of the
    /// same session id is dropped.
    pub fn refresh_session_once(&self, session: &str, now: i64) -> Result<Pass, ClaimError> {
        self.forget_ended(session)?;
        self.once_per_programme(session, now, ONCE_GRACE, |lock| {
            lock.refresh_work(session, now)
        })
    }

    /// Deletes every `work` claim of an ended session and keeps its
    /// `handoff` claims, waiting for the programme locks no longer than
    /// [`ONCE_GRACE`] for the whole pass. When a lock stayed busy, or a
    /// programme failed, the claims there stay, `ended/<session>` is written
    /// (without a lock) and the next locked write in a programme applies it.
    pub fn end_session_once(&self, session: &str, now: i64) -> Result<Pass, ClaimError> {
        self.end_session_within(session, now, ONCE_GRACE)
    }

    /// [`Store::end_session_once`] with the pass's lock wait given.
    pub(crate) fn end_session_within(
        &self,
        session: &str,
        now: i64,
        grace: Duration,
    ) -> Result<Pass, ClaimError> {
        let pass =
            self.once_per_programme(session, now, grace, |lock| lock.remove_work(session, now))?;
        if pass.skipped > 0 {
            self.mark_ended(session, now)?;
        }
        Ok(pass)
    }

    fn once_per_programme(
        &self,
        session: &str,
        now: i64,
        grace: Duration,
        act: impl Fn(&SlugLock<'_>) -> Result<usize, ClaimError>,
    ) -> Result<Pass, ClaimError> {
        let mut pass = Pass::default();
        let deadline = Instant::now() + grace;
        for key in self.keys()? {
            let step = || -> Result<Option<usize>, ClaimError> {
                if !self.has_work_claim_in(&key, session)? {
                    return Ok(Some(0));
                }
                let wait = deadline.saturating_duration_since(Instant::now());
                match self.lock(&key, wait, now) {
                    Ok(lock) => act(&lock).map(Some),
                    Err(ClaimError::LockTimeout { .. }) => {
                        self.log(Event::Skipped, &key, "*", Some((session, Kind::Work)), now)?;
                        Ok(None)
                    }
                    Err(error) => Err(error),
                }
            };
            match step() {
                Ok(Some(done)) => pass.done += done,
                Ok(None) => pass.skipped += 1,
                Err(error) => {
                    pass.skipped += 1;
                    pass.failures.push((key, error));
                }
            }
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
#[derive(Debug, Default)]
pub struct Pass {
    /// Claims refreshed or deleted.
    pub done: usize,
    /// Programmes whose lock was busy or that failed, and were left alone.
    pub skipped: usize,
    /// The programmes that failed, with why.
    pub failures: Vec<(ClaimKey, ClaimError)>,
}

impl Pass {
    /// One line per failed programme, naming it and the cause.
    pub fn failure_lines(&self) -> Vec<String> {
        self.failures
            .iter()
            .map(|(key, error)| format!("claims {}/{}: {error}", key.repo_key, key.slug))
            .collect()
    }
}

/// A programme's claim lock, held: the only way to write a claim.
#[derive(Debug)]
pub struct SlugLock<'a> {
    _lock: LockFile,
    store: &'a Store,
    key: ClaimKey,
    dir: PathBuf,
}

/// How a ticket came to be claimed as `work`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acquired {
    /// No live claim held it.
    Created,
    /// A `handoff` claim became the session's `work`.
    TookOver,
    /// The session already held it as `work`.
    Refreshed,
    /// Another session's live `work` claim was taken.
    Stole,
}

/// What asking for a ticket as `work` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Taken {
    /// The claim was written; the event says how.
    Claimed {
        how: Acquired,
        /// The holder the ticket came from: the handoff it took over, or the
        /// claim it stole; `None` when it was created or refreshed.
        from: Option<Holder>,
    },
    /// Another live session holds the ticket as `work`.
    Refused { holder: Holder, age_secs: u64 },
}

/// What releasing a ticket came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Released {
    Released,
    /// There was no claim to release.
    Absent,
    /// Another session holds it; nothing was deleted.
    NotHolder {
        holder: Holder,
    },
}

impl SlugLock<'_> {
    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.toml"))
    }

    fn write(&self, id: &str, claim: &Claim) -> Result<(), ClaimError> {
        let path = self.path(id);
        let text = toml::to_string(claim).map_err(|source| ClaimError::Encode {
            path: path.clone(),
            source,
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

    /// The ids this programme's claims hold at `now`, read under the lock.
    pub fn held_ids(&self, now: i64) -> Result<BTreeSet<String>, ClaimError> {
        Ok(read_all(&self.dir)?
            .into_iter()
            .filter(|(_, record)| held(record, now))
            .map(|(id, _)| id)
            .collect())
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
    /// a takeover of a `handoff`, or (with [`Contest::Steal`]) another session's `work`.
    pub fn claim_work(
        &self,
        id: &str,
        session: &str,
        repo_display: &str,
        contest: Contest,
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
        // `previous` is the holder the ticket came from, as `Taken` reports it.
        let (claim, event, previous) = match &existing {
            None => (fresh(None, now), Event::Create, None),
            Some(record) if !held(record, now) => {
                self.log_record(Event::Expire, id, record, now)?;
                (fresh(None, now), Event::Create, None)
            }
            Some(ClaimRecord::Parsed(held_claim)) if held_claim.session == session => {
                let event = match held_claim.kind {
                    Kind::Work => Event::Refresh,
                    Kind::Handoff => Event::Takeover,
                };
                (
                    fresh(held_claim.from.clone(), held_claim.claimed_at),
                    event,
                    None,
                )
            }
            Some(
                record @ ClaimRecord::Parsed(Claim {
                    kind: Kind::Handoff,
                    session: handed_by,
                    ..
                }),
            ) => (
                fresh(Some(handed_by.clone()), now),
                Event::Takeover,
                Some(record.holder()),
            ),
            Some(record) => match contest {
                Contest::Refuse => {
                    return Ok(Taken::Refused {
                        holder: record.holder(),
                        age_secs: record.age_secs(now),
                    });
                }
                Contest::Steal => {
                    let holder = record.holder();
                    let from = match &holder {
                        Holder::Session { id, .. } => Some(id.clone()),
                        Holder::Unreadable => None,
                    };
                    (fresh(from, now), Event::Steal, Some(holder))
                }
            },
        };
        self.write(id, &claim)?;
        self.log(event, id, Some((session, Kind::Work)), now)?;
        let how = match event {
            Event::Takeover => Acquired::TookOver,
            Event::Refresh => Acquired::Refreshed,
            Event::Steal => Acquired::Stole,
            Event::Create | Event::Release | Event::Sweep | Event::Expire | Event::Skipped => {
                Acquired::Created
            }
        };
        Ok(Taken::Claimed {
            how,
            from: previous,
        })
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
                holder: record.holder(),
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

/// Whether a session id is safe to use as one file name.
fn is_file_name(session: &str) -> bool {
    !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
