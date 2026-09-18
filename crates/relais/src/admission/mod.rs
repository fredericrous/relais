//! Admission control (SPEC §23).
//!
//! Separate limits for active remote model work, local heavy commands,
//! indexing and training; global, per-session and per-run caps; fair
//! scheduling with queue aging so one tab cannot starve the others. Managed
//! dispatch reserves capacity and budget atomically before launch, and a
//! retry with the same dispatch ID cannot create duplicate agents. Parents
//! waiting on children relinquish active-execution capacity so all slots
//! cannot be held by waiters.
