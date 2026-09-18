//! Policy and authority (SPEC §5).
//!
//! Repository policy lives in `relais.toml`; machine-owned settings (under
//! `~/.config/relais/`) hold spending ceilings, allowed providers/models,
//! permissions and trust grants. Effective authority is the intersection of
//! repo policy, machine settings and per-run options — a per-run option can
//! narrow a grant but never broaden one. Trust grants are content-bound to
//! reviewed execution profiles; a changed declaration invalidates them.
//! Dependencies are required, optional or off, and optional gaps are
//! reported, never turned into passes.
