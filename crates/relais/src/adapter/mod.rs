//! Execution backends (SPEC §8, §20).
//!
//! The mandatory Claude Code adapter launches `claude -p` child processes
//! with explicit model, effort, turn limits and budget controls, passing
//! prompts via stdin and arguments as an argv array. The installed version
//! is capability-checked against a compatibility matrix before any launch;
//! missing permissions produce a blocked result and no bypass flags are
//! introduced. A backend interface admits alternative providers without
//! requiring every possible provider to ship.
