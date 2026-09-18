//! Owned task worktrees (SPEC §8).
//!
//! The base commit is resolved once and an owned worktree is created from
//! that exact SHA; the user's checkout and other worktrees stay untouched.
//! Write scope is checked on the actual diff after each attempt — a scope
//! violation cannot be accepted. This is an acceptance boundary, not
//! filesystem isolation: tools with Bash access are not a sandbox. The
//! runner records an immutable candidate snapshot outside model control,
//! and a worktree with unexported changes is never force-cleaned.
