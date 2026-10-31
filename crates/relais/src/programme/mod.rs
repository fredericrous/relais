//! Programme maps (SPEC §31): `docs/programmes/<slug>/map.md` plus one
//! TOML-frontmatter ticket file per decision, read and queried, never
//! written. Parsing and analysis are pure; only [`load`] touches the
//! filesystem and git, and nothing here opens the ledger.

pub mod analysis;
pub mod load;
pub mod model;
pub mod parse;
