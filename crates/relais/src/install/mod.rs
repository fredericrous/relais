//! Claude Code integration (SPEC §3).
//!
//! `relais install --claude` proposes a namespaced set of agent definitions
//! and a `/relais` skill. Preview-first: `--write` applies reviewed
//! changes, merging configuration without replacing unrelated entries;
//! uninstall removes only owned, unchanged artifacts. Native agent
//! definitions are advisory defaults — the runner, not a frontmatter file,
//! owns spending policy.
