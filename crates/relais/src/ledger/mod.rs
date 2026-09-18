//! The machine-local ledger (SPEC §12).
//!
//! SQLite for records (tasks, contract revisions, attempts, transitions,
//! evidence, usage events, outcomes, dispatch intents), an artifact
//! directory per run, explicit payload schema versions with additive
//! migrations. Dispatch intent is persisted before spawning a process;
//! on restart liveness, terminal output and artifacts are reconciled
//! before another attempt is scheduled. An absent terminal result never
//! means nothing executed. Raw transcripts are opt-in; no credentials or
//! private reasoning are retained.
