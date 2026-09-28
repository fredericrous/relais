//! Orchestration usage (SPEC §11): what the orchestrating Claude Code
//! session itself spent — writing contracts, reviewing candidates,
//! landing PRs — priced from a machine-owned price table.
//!
//! Pure: parsing a transcript line and pricing a usage record are both
//! functions of bytes already in hand, never of the filesystem, the
//! clock or the environment. Locating transcript files on disk, reading
//! them and persisting the result belongs to the caller (`ledger` and
//! `main`), not here — that is what keeps this module on the purity
//! list in `scripts/check-module-cycles.py`.

pub mod pricing;
pub mod transcript;

pub use pricing::{price, ModelPrice, PriceTable, Priced};
pub use transcript::{parse_transcript, CacheWrites, Speed, TranscriptSource, UsageRecord};
