//! Strong identities and content hashes (SPEC §4, §7, §12).
//!
//! Every recorded entity has an identifier that is unique per process and
//! sortable enough for humans, and evidence is keyed by content hash so a
//! receipt can name the exact candidate it was bound to.

use std::sync::atomic::{AtomicU64, Ordering};

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

fn unix_micros() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the unix epoch")
        .as_micros()
}

fn process_id() -> u32 {
    std::process::id()
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Run identifier: unique per process, time-ordered, human-typable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunId(String);

impl RunId {
    pub fn generate() -> Self {
        Self(format!(
            "run-{:x}-{:x}",
            unix_micros(),
            process_id() as u128 ^ (COUNTER.fetch_add(1, Ordering::Relaxed) as u128) << 32
        ))
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DispatchId(String);

impl DispatchId {
    pub fn generate() -> Self {
        Self(format!(
            "disp-{:x}-{:x}",
            unix_micros(),
            process_id() as u128 ^ (COUNTER.fetch_add(1, Ordering::Relaxed) as u128) << 32
        ))
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
    fn run_ids_are_unique_within_a_process() {
        let a = RunId::generate();
        let b = RunId::generate();
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("run-"));
    }
}
