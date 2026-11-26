+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Whether relais's host scheduling duplicates greengate's slot queue

## Question

Do `admission`, `coordinator` and `procs` in relais duplicate greengate's
(was amont) host slot queue (ADR-0014), and can one of them go?

## Look at

- `crates/relais/src/admission/mod.rs` (6.4k lines), `admission/native.rs`,
  `crates/relais/src/coordinator/mod.rs` (5.3k lines) and
  `crates/relais/src/procs.rs`.
- `greengate slots --json` and its documentation in the greengate repository
  (`amont`'s `docs/`), and decisions ADR-0014 (one host slot queue) and
  ADR-0013 (a busy host defers).
- The memory notes `amont_one_host_slot_queue_adr_0014` and
  `relais_settle_timeouts_adr_0012`, for what is already decided.

## Answer looks like

A table with one row per relais type or function that admits, queues, waits
or reaps: what it decides, the greengate surface that decides the same thing
(or none), and whether it is removed, kept, or moved. Then one line: what
relais still has to own after the overlap is gone, and how many lines that
removes from the crate.
