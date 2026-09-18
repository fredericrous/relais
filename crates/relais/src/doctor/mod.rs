//! Environment diagnosis (SPEC §3: `relais doctor`).
//!
//! Checks the Claude Code binary and its version against the compatibility
//! matrix, git, the aval/amont/amont-agent integrations and their
//! versions, config parsing, coordinator reachability, ledger migrations
//! and registry state. Findings distinguish hard blockers from warnings;
//! nothing here repairs anything by itself.
