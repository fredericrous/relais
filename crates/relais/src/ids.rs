//! Strong identities and content hashes (SPEC §4, §7, §12).
//!
//! Every recorded entity has an identifier that is unique per process and
//! sortable enough for humans, and evidence is keyed by content hash so a
//! receipt can name the exact candidate it was bound to.
//!
//! Minting an identifier needs a clock, a process id and a sequence.
//! Those are inputs, not ambient state: an [`IdSource`] is built once at
//! a boundary and passed to whatever mints ids, so a test can script the
//! clock and two sources in one process cannot share a counter by
//! accident.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Content-addressed hash of canonical JSON, hex-encoded.
///
/// `serde_json::Value` orders object keys (BTreeMap default), so serializing
/// a parsed value gives sorted-key, byte-stable output — the canonical form
/// a frozen contract hashes to. Contracts must not contain floats: JSON
/// numbers round-trip through the same representation, but the format's only
/// freedom is there, so the contract schema keeps its numbers integers.
pub fn canonical_json_hash(value: &serde_json::Value) -> String {
    let canonical = serde_json::to_vec(value).expect("serde_json::Value serializes");
    to_hex(&Sha256::digest(&canonical))
}

/// SHA-256 of raw bytes, hex-encoded.
pub fn sha256_hex(data: &[u8]) -> String {
    to_hex(&Sha256::digest(data))
}

pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Why an identifier could not be minted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    /// The clock answered a time before 1970. An identifier derived from
    /// it would not be time-ordered, and a machine whose clock is that
    /// wrong has a problem the run cannot fix — so this is an error the
    /// caller reports, never a panic in the middle of a dispatch.
    ClockBeforeEpoch,
}

impl std::fmt::Display for IdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClockBeforeEpoch => write!(
                f,
                "the system clock answered a time before the unix epoch, so no \
                 time-ordered identifier can be minted; fix the machine's clock"
            ),
        }
    }
}

impl std::error::Error for IdError {}

/// Where identifiers get their uniqueness: a clock, a process id and a
/// sequence, all supplied rather than read from the ambient process.
/// One is built at a boundary (`IdSource::of_this_process` in `main`)
/// and passed to everything that mints ids.
pub struct IdSource {
    clock: Box<dyn Fn() -> SystemTime + Send + Sync>,
    process: u32,
    next: AtomicU64,
}

impl IdSource {
    /// This process's source: the wall clock and this process's id.
    /// The one place in the crate that reads either.
    pub fn of_this_process() -> Self {
        Self::new(SystemTime::now, std::process::id())
    }

    /// A source on a supplied clock and process id — what a test uses to
    /// make minted identifiers a value it can assert on.
    pub fn new(clock: impl Fn() -> SystemTime + Send + Sync + 'static, process: u32) -> Self {
        Self {
            clock: Box::new(clock),
            process,
            next: AtomicU64::new(0),
        }
    }

    fn mint(&self, prefix: &str) -> Result<String, IdError> {
        let micros = (self.clock)()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| IdError::ClockBeforeEpoch)?
            .as_micros();
        let sequence = self.next.fetch_add(1, Ordering::Relaxed);
        Ok(format!(
            "{prefix}-{:x}-{:x}",
            micros,
            self.process as u128 ^ (sequence as u128) << 32
        ))
    }

    /// A fresh run identifier.
    pub fn run_id(&self) -> Result<RunId, IdError> {
        Ok(RunId(self.mint("run")?))
    }

    /// A fresh dispatch identifier.
    pub fn dispatch_id(&self) -> Result<DispatchId, IdError> {
        Ok(DispatchId(self.mint("disp")?))
    }
}

/// Run identifier: unique per source, time-ordered, human-typable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    /// An identifier that already exists: read back from the ledger, or
    /// typed by a human on the command line. Nothing is validated —
    /// whether a run by that name exists is the ledger's answer, not a
    /// property of the string.
    pub fn from_stored(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Dispatch identifier: one model launch, reserved atomically before the
/// process exists (SPEC §12: persist intent before spawning). Retrying with
/// the same ID cannot create duplicate agents (SPEC §23).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DispatchId(String);

impl DispatchId {
    /// A dispatch identifier read back from the ledger or given on the
    /// command line. See [`RunId::from_stored`].
    pub fn from_stored(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DispatchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A work package's identifier inside one decomposed run (SPEC §19).
/// Validated where a plan is parsed (`WorkPlan::validate`), because a
/// package id names a directory on disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageId(String);

impl PackageId {
    pub fn from_stored(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PackageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A process id as the operating system reports it. The ledger stores it
/// as a signed integer, so reading one back is fallible: `Pid::stored`
/// says so rather than folding "no pid recorded" together with "a number
/// no process can have".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pid(u32);

impl Pid {
    pub fn new(pid: u32) -> Self {
        Self(pid)
    }

    /// A pid as an integer column holds it. `None` when the value is
    /// negative or wider than a pid, which no operating system reports
    /// and only a corrupt row contains.
    pub fn stored(raw: i64) -> Option<Self> {
        u32::try_from(raw).ok().map(Self)
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for Pid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn canonical_hash_is_key_order_insensitive() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"a":1,"b":{"d":2,"c":3}}"#).expect("parse");
        let b: serde_json::Value =
            serde_json::from_str(r#"{"b":{"c":3,"d":2},"a":1}"#).expect("parse");
        assert_eq!(canonical_json_hash(&a), canonical_json_hash(&b));
    }

    #[test]
    fn canonical_hash_distinguishes_content() {
        let a: serde_json::Value = serde_json::from_str(r#"{"a":1}"#).expect("parse");
        let b: serde_json::Value = serde_json::from_str(r#"{"a":2}"#).expect("parse");
        assert_ne!(canonical_json_hash(&a), canonical_json_hash(&b));
    }

    #[test]
    fn run_ids_are_unique_within_a_source() {
        let ids = IdSource::of_this_process();
        let a = ids.run_id().expect("run id");
        let b = ids.run_id().expect("run id");
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("run-"));
        assert!(ids
            .dispatch_id()
            .expect("dispatch id")
            .as_str()
            .starts_with("disp-"));
    }

    /// The format is the ledger's wire format: a supplied clock and
    /// process id produce exactly the string the ambient ones used to.
    #[test]
    fn a_supplied_clock_and_pid_produce_the_same_id_format() {
        let at = UNIX_EPOCH + std::time::Duration::from_micros(0x1234_5678);
        let ids = IdSource::new(move || at, 0x99);
        assert_eq!(ids.run_id().expect("id").as_str(), "run-12345678-99");
        assert_eq!(
            ids.dispatch_id().expect("id").as_str(),
            "disp-12345678-100000099",
            "the sequence moves into the high half, as it always did"
        );
    }

    /// C (ids.rs): a clock before the epoch used to abort the process
    /// inside `expect`. It is a machine fault the caller reports.
    #[test]
    fn a_pre_epoch_clock_is_an_error_not_a_panic() {
        let before = UNIX_EPOCH - std::time::Duration::from_secs(1);
        let ids = IdSource::new(move || before, 1);
        assert_eq!(ids.run_id(), Err(IdError::ClockBeforeEpoch));
        assert_eq!(ids.dispatch_id(), Err(IdError::ClockBeforeEpoch));
        assert!(IdError::ClockBeforeEpoch.to_string().contains("clock"));
    }

    #[test]
    fn a_stored_pid_rejects_what_no_process_can_be() {
        assert_eq!(Pid::stored(4242).map(Pid::get), Some(4242));
        assert_eq!(Pid::stored(-1), None, "no process has a negative id");
        assert_eq!(Pid::stored(i64::from(u32::MAX) + 1), None);
    }
}
