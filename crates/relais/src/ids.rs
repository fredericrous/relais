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
/// One is built at a boundary (`main::id_source`) and passed to
/// everything that mints ids; this module names neither the clock nor
/// the process table (`effects.no-ambient-access`).
pub struct IdSource {
    clock: Box<dyn Fn() -> SystemTime + Send + Sync>,
    process: u32,
    next: AtomicU64,
}

impl IdSource {
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

/// A task identifier: the stable identity that survives contract
/// revisions and re-runs. Unlike [`RunId`] and [`DispatchId`], it is
/// never minted from a clock — [`derive_task_id`] derives it from the
/// repository and the task's first contract, so the same task dispatched
/// twice (or revised) is still one task.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(String);

impl TaskId {
    /// A task identifier read back from the ledger. See [`RunId::from_stored`].
    pub fn from_stored(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A task's identity, derived — not minted — from the repository it runs
/// in and the contract its task first dispatched under. Pure: the same
/// two inputs always produce the same [`TaskId`], so a re-run or a
/// `--revise` of the same task lands on the same identity without the
/// ledger being consulted. `repo_key` is `policy::repo_key`'s output;
/// this module names no other module's types, so it arrives as a plain
/// string rather than a `RepoIdentity`.
pub fn derive_task_id(repo_key: &str, first_contract_hash: &str) -> TaskId {
    let hash = sha256_hex(format!("{repo_key}:{first_contract_hash}").as_bytes());
    TaskId(format!("task-{}", &hash[..16]))
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

/// A harness session's identifier, as the hook payload names it. Read
/// from a payload, never minted here — the harness owns the value, this
/// crate only carries it far enough that a session, a tool use and a
/// prompt cannot be swapped for one another at a call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A hook payload's `prompt_id`: identical across every event of one
/// turn (measured, not assumed — see the hook fixtures), which is what
/// a later package needs to narrow a correlation to one turn.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PromptId(String);

impl PromptId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PromptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A tool call's identifier, as the harness names it in `tool_use_id`.
/// Scoped to one `PreToolUse`/`PostToolUse` pair, never to an agent or
/// a session, so a function that takes a tool use and an agent cannot
/// have the two swapped.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolUseId(String);

impl ToolUseId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ToolUseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A dispatch's identity, derived — not minted — from the session that
/// requested it and the tool use it answers. Pure: the same session and
/// tool use always derive the same [`DispatchId`], reading no clock, no
/// counter and no environment. This is what lets a hook that fires twice
/// for one tool call (a retried delivery, not two requests) recognise
/// its second firing as the same dispatch rather than admit it again —
/// the recognition [`IdSource::dispatch_id`]'s clock-and-sequence mint
/// cannot give, because two calls to it never agree.
pub fn derive_dispatch_id(session: &SessionId, tool_use: &ToolUseId) -> DispatchId {
    // Hashed as a structured value, not as two strings with a separator
    // between them. `format!("{a}:{b}")` cannot say where the first
    // input ended, so ("a:b", "c") and ("a", "b:c") hash identically —
    // demonstrated, not hypothesised — and neither newtype validates
    // its contents. Claude Code's own ids carry no colon today, which
    // makes this a structural ambiguity rather than a live collision,
    // and a structural ambiguity in an identity is not worth keeping
    // when the crate already has a composite hash that cannot have one.
    let hash = canonical_json_hash(&serde_json::json!({
        "session": session.as_str(),
        "tool_use": tool_use.as_str(),
    }));
    DispatchId(format!("disp-{}", &hash[..16]))
}

/// A run's identity, derived — not minted — from the session it belongs
/// to. Pure for the same reason [`derive_dispatch_id`] is.
pub fn derive_run_id(session: &SessionId) -> RunId {
    let hash = sha256_hex(format!("run:{}", session.as_str()).as_bytes());
    RunId(format!("run-{}", &hash[..16]))
}

/// A subagent's identifier at runtime, as the harness's `agent_id`
/// names it — distinct from [`AgentType`], which names the KIND of
/// agent (`general-purpose`, …), not the one running instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An agent's type name (`agent_type` in the payload, e.g.
/// `general-purpose`) — the kind dispatched, not the running instance.
/// See [`AgentId`] for that.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentType(String);

impl AgentType {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
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
        let ids = IdSource::new(SystemTime::now, std::process::id());
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
    fn task_ids_are_pure_and_shaped_task_dash_16_hex() {
        let a = derive_task_id("repo-key", "hash-1");
        let b = derive_task_id("repo-key", "hash-1");
        assert_eq!(a, b, "the same inputs always derive the same task");
        assert!(a.as_str().starts_with("task-"));
        assert_eq!(a.as_str().len(), "task-".len() + 16);
        let different_repo = derive_task_id("other-repo", "hash-1");
        let different_contract = derive_task_id("repo-key", "hash-2");
        assert_ne!(a, different_repo);
        assert_ne!(a, different_contract);
    }

    #[test]
    fn dispatch_ids_are_pure_and_a_repeat_delivery_matches() {
        let session = SessionId::new("session-a");
        let tool_use = ToolUseId::new("toolu-01");
        let first = derive_dispatch_id(&session, &tool_use);
        let second = derive_dispatch_id(&session, &tool_use);
        assert_eq!(
            first, second,
            "a hook that fires twice for one tool call derives the same dispatch"
        );
        let different_tool_use = derive_dispatch_id(&session, &ToolUseId::new("toolu-02"));
        let different_session = derive_dispatch_id(&SessionId::new("session-b"), &tool_use);
        assert_ne!(first, different_tool_use);
        assert_ne!(first, different_session);

        // Where the first input ends must be unambiguous. Concatenating
        // with a separator could not say: `("a:b", "c")` and
        // `("a", "b:c")` both rendered `a:b:c` and derived ONE id —
        // demonstrated, not hypothesised. Neither newtype validates its
        // contents, so nothing but this keeps the two apart.
        let split_left = derive_dispatch_id(&SessionId::new("a:b"), &ToolUseId::new("c"));
        let split_right = derive_dispatch_id(&SessionId::new("a"), &ToolUseId::new("b:c"));
        assert_ne!(
            split_left, split_right,
            "two different (session, tool use) pairs must not share a dispatch identity"
        );
    }

    #[test]
    fn run_ids_derived_from_a_session_are_pure() {
        let session = SessionId::new("session-a");
        let first = derive_run_id(&session);
        let second = derive_run_id(&session);
        assert_eq!(
            first, second,
            "the same session always derives the same run"
        );
        assert_ne!(first, derive_run_id(&SessionId::new("session-b")));
    }

    #[test]
    fn a_stored_pid_rejects_what_no_process_can_be() {
        assert_eq!(Pid::stored(4242).map(Pid::get), Some(4242));
        assert_eq!(Pid::stored(-1), None, "no process has a negative id");
        assert_eq!(Pid::stored(i64::from(u32::MAX) + 1), None);
    }

    proptest::proptest! {
        /// A contract hash is a function of the VALUE, not of the order
        /// the keys happened to arrive in: `serde_json::Value` sorts its
        /// maps, so the same document written two ways hashes alike.
        /// This is what makes a trust grant content-bound (SPEC §5).
        #[test]
        fn canonical_hashing_ignores_key_order_at_every_depth(
            keys in proptest::collection::vec("[a-z]{1,6}", 1..6),
            values in proptest::collection::vec(0i64..1000, 1..6),
            depth in 0usize..4,
        ) {
            use proptest::prelude::*;
            // Distinct keys: a document that names one key twice is not
            // one document written two ways, it is two documents.
            let mut seen = std::collections::BTreeSet::new();
            let pairs: Vec<(String, i64)> = keys
                .iter()
                .cloned()
                .zip(values.iter().copied())
                .filter(|(key, _)| seen.insert(key.clone()))
                .collect();
            let build = |order: &mut dyn Iterator<Item = &(String, i64)>| {
                let mut map = serde_json::Map::new();
                for (key, value) in order {
                    map.insert(key.clone(), serde_json::json!(value));
                }
                serde_json::Value::Object(map)
            };
            let forwards = build(&mut pairs.iter());
            let backwards = build(&mut pairs.iter().rev());
            // …and nested that many levels deep, which is where a
            // hand-written canonicalizer would have stopped sorting.
            let nest = |mut inner: serde_json::Value| {
                for level in 0..depth {
                    inner = serde_json::json!({ format!("level{level}"): inner });
                }
                inner
            };
            prop_assert_eq!(
                canonical_json_hash(&nest(forwards)),
                canonical_json_hash(&nest(backwards))
            );
        }

        /// And it separates: a different value is a different hash.
        #[test]
        fn a_different_document_hashes_differently(left in 0i64..1000, right in 0i64..1000) {
            use proptest::prelude::*;
            let hash = |n: i64| canonical_json_hash(&serde_json::json!({ "n": n }));
            prop_assert_eq!(hash(left) == hash(right), left == right);
        }
    }
}
