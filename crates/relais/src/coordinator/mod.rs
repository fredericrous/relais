//! The shared per-user coordinator (SPEC §23).
//!
//! One coordinator per OS user, started lazily and elected atomically over
//! a permission-restricted local Unix socket. It owns registrations,
//! admission, resource leases and aggregate accounting, and shares one
//! model registry and ledger. A CLI process exiting never cancels an
//! ongoing background run. No database write transaction stays open during
//! a model call, build or training job. Session, run, work package,
//! attempt, agent, parent agent, invocation, repository, worktree and
//! candidate are identified separately.
