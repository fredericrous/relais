//! The record that a probe session showed the sandbox holds, and what it is
//! keyed by.
//!
//! A pass is worth exactly the configuration it was measured under, so the
//! key is a hash of everything that configuration is: the harness version,
//! the platform, the settings the worker will launch with and the managed
//! files that could weaken them. Change any of it and the record no longer
//! matches, so the sandbox is probed again rather than assumed.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::fsutil::write_atomic;
use crate::ids::to_hex;

const SCRATCH_PLACEHOLDER: &str = "<scratch>";

/// Hex SHA-256 of a verified configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationKey(String);

impl VerificationKey {
    /// The scratch path is replaced by `<scratch>` in `settings` first: it
    /// differs per attempt and says nothing about how the sandbox behaves.
    pub fn compute(
        harness_version: &str,
        platform: &str,
        settings: &Value,
        scratch: &Path,
        managed: &[(PathBuf, Vec<u8>)],
    ) -> VerificationKey {
        let scratch = scratch.to_string_lossy();
        let settings = abstract_scratch(settings, &scratch).to_string();

        let mut managed: Vec<&(PathBuf, Vec<u8>)> = managed.iter().collect();
        managed.sort_by(|a, b| a.0.cmp(&b.0));

        let mut hasher = Sha256::new();
        feed(&mut hasher, harness_version.as_bytes());
        feed(&mut hasher, platform.as_bytes());
        feed(&mut hasher, settings.as_bytes());
        for (path, bytes) in managed {
            feed(&mut hasher, path.to_string_lossy().as_bytes());
            feed(&mut hasher, bytes);
        }
        VerificationKey(to_hex(&hasher.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Length-prefixed, so `("ab", "c")` and `("a", "bc")` hash differently.
fn feed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// `value` with `scratch` replaced in every string and object key. Objects
/// are rebuilt in a `Map`, which keeps keys sorted, so the serialisation is
/// canonical.
fn abstract_scratch(value: &Value, scratch: &str) -> Value {
    let swap = |text: &str| {
        if scratch.is_empty() {
            text.to_string()
        } else {
            text.replace(scratch, SCRATCH_PLACEHOLDER)
        }
    };
    match value {
        Value::String(text) => Value::String(swap(text)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| abstract_scratch(item, scratch))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (swap(key), abstract_scratch(item, scratch)))
                .collect::<Map<_, _>>(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

/// One passed probe: the key it holds for and the report that earned it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRecord {
    pub key: String,
    pub verified_at: String,
    pub harness_version: String,
    pub platform: String,
    /// The rendered report lines.
    pub report: Vec<String>,
}

/// Why a store could not be loaded or saved.
#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    /// The file exists but is not a store. Never read as empty: an empty
    /// store would quietly discard every verification it held.
    Corrupt(serde_json::Error),
    /// The records could not be turned into text; the file on disk is fine.
    Serialize(serde_json::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io(err) => write!(f, "verification store: {err}"),
            StoreError::Corrupt(err) => write!(f, "verification store is corrupt: {err}"),
            StoreError::Serialize(err) => {
                write!(f, "verification store could not be serialised: {err}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(err: io::Error) -> Self {
        StoreError::Io(err)
    }
}

/// The verification records on disk at one path.
#[derive(Debug)]
pub struct VerificationStore {
    path: PathBuf,
    records: Vec<VerificationRecord>,
}

impl VerificationStore {
    /// A missing file is an empty store; an unreadable or corrupt one is an
    /// error.
    pub fn load(path: &Path) -> Result<Self, StoreError> {
        let records = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(StoreError::Corrupt)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => return Err(StoreError::Io(err)),
        };
        Ok(VerificationStore {
            path: path.to_path_buf(),
            records,
        })
    }

    pub fn find(&self, key: &VerificationKey) -> Option<&VerificationRecord> {
        self.records.iter().find(|record| record.key == key.0)
    }

    /// Adds `record`, replacing the one with the same key.
    pub fn record(&mut self, record: VerificationRecord) {
        match self.records.iter_mut().find(|held| held.key == record.key) {
            Some(held) => *held = record,
            None => self.records.push(record),
        }
    }

    /// Writes through [`write_atomic`], so a reader sees the old records or
    /// the new, never half of either.
    pub fn save(&self) -> Result<(), StoreError> {
        let text = serde_json::to_string_pretty(&self.records).map_err(StoreError::Serialize)?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_atomic(&self.path, &text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::temp_dir;

    fn settings(scratch: &str) -> Value {
        json!({
            "sandbox": {"filesystem": {"allowWrite": [scratch, "/other"]}},
            "permissions": {"deny": [format!("Read({scratch}/x)")]},
        })
    }

    fn managed() -> Vec<(PathBuf, Vec<u8>)> {
        vec![
            (PathBuf::from("/etc/a.json"), b"{}".to_vec()),
            (PathBuf::from("/etc/b.json"), b"[]".to_vec()),
        ]
    }

    fn key(version: &str, platform: &str, settings: &Value, scratch: &str) -> VerificationKey {
        VerificationKey::compute(version, platform, settings, Path::new(scratch), &managed())
    }

    fn base() -> VerificationKey {
        key("2.1.290", "macos", &settings("/s/1"), "/s/1")
    }

    #[test]
    fn the_key_is_stable_for_equal_inputs() {
        assert_eq!(base(), base());
        assert_eq!(base().as_str().len(), 64);
    }

    #[test]
    fn the_key_ignores_the_scratch_path() {
        assert_eq!(base(), key("2.1.290", "macos", &settings("/s/2"), "/s/2"));
    }

    #[test]
    fn the_key_changes_with_version_platform_settings_and_managed_bytes() {
        let s = settings("/s/1");
        assert_ne!(base(), key("2.1.291", "macos", &s, "/s/1"));
        assert_ne!(base(), key("2.1.290", "linux", &s, "/s/1"));

        let mut changed = s.clone();
        changed["sandbox"]["filesystem"]["allowWrite"][1] = json!("/another");
        assert_ne!(base(), key("2.1.290", "macos", &changed, "/s/1"));

        let mut bytes = managed();
        bytes[1].1 = b"[1]".to_vec();
        let other = VerificationKey::compute("2.1.290", "macos", &s, Path::new("/s/1"), &bytes);
        assert_ne!(base(), other);
    }

    #[test]
    fn the_key_sees_which_managed_file_holds_which_bytes() {
        let s = settings("/s/1");
        let swapped = vec![
            (PathBuf::from("/etc/a.json"), b"[]".to_vec()),
            (PathBuf::from("/etc/b.json"), b"{}".to_vec()),
        ];
        let other = VerificationKey::compute("2.1.290", "macos", &s, Path::new("/s/1"), &swapped);
        assert_ne!(base(), other);
    }

    #[test]
    fn the_key_does_not_depend_on_managed_file_order() {
        let s = settings("/s/1");
        let mut reversed = managed();
        reversed.reverse();
        let other = VerificationKey::compute("2.1.290", "macos", &s, Path::new("/s/1"), &reversed);
        assert_eq!(base(), other);
    }

    #[test]
    fn the_key_does_not_depend_on_settings_key_order() {
        let a: Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        assert_eq!(key("v", "p", &a, "/s"), key("v", "p", &b, "/s"));
    }

    fn record(key: &str, at: &str) -> VerificationRecord {
        VerificationRecord {
            key: key.to_string(),
            verified_at: at.to_string(),
            harness_version: "2.1.290".to_string(),
            platform: "macos".to_string(),
            report: vec!["✓ pipe: ok".to_string(), "✓ init: ok".to_string()],
        }
    }

    #[test]
    fn a_store_round_trips() {
        let dir = temp_dir("verified-roundtrip");
        let path = dir.join("sandbox").join("verified.json");
        let mut store = VerificationStore::load(&path).unwrap();
        store.record(record(base().as_str(), "2026-09-30T10:00:00Z"));
        store.save().unwrap();

        let loaded = VerificationStore::load(&path).unwrap();
        assert_eq!(
            loaded.find(&base()),
            Some(&record(base().as_str(), "2026-09-30T10:00:00Z"))
        );
        assert!(loaded
            .find(&key("other", "macos", &json!({}), "/s"))
            .is_none());
    }

    #[test]
    fn a_missing_file_is_an_empty_store() {
        let dir = temp_dir("verified-missing");
        let store = VerificationStore::load(&dir.join("verified.json")).unwrap();
        assert!(store.find(&base()).is_none());
    }

    #[test]
    fn a_corrupt_file_is_an_error() {
        let dir = temp_dir("verified-corrupt");
        let path = dir.join("verified.json");
        std::fs::write(&path, "{ not a store").unwrap();
        assert!(matches!(
            VerificationStore::load(&path),
            Err(StoreError::Corrupt(_))
        ));
    }

    #[test]
    fn recording_a_key_again_replaces_its_record() {
        let dir = temp_dir("verified-replace");
        let path = dir.join("verified.json");
        let mut store = VerificationStore::load(&path).unwrap();
        store.record(record(base().as_str(), "2026-09-30T10:00:00Z"));
        store.record(record(base().as_str(), "2026-10-01T10:00:00Z"));
        store.save().unwrap();

        let loaded = VerificationStore::load(&path).unwrap();
        assert_eq!(loaded.records.len(), 1);
        assert_eq!(
            loaded.find(&base()).unwrap().verified_at,
            "2026-10-01T10:00:00Z"
        );
    }
}
