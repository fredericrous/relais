---
canonical: fredericrous/decisions:docs/plans/2026-11-12-greengate-signs-notes-under-green-names.md
phases: [2, 4, 5]
status: active
---
Part of [greengate signs its notes under green names](https://github.com/fredericrous/decisions/blob/main/docs/plans/2026-11-12-greengate-signs-notes-under-green-names.md) (ADR-0068): this repository takes Phase 2, `relais native gate-stamp` reading `refs/notes/greengate-gate` (header `greengate-gate-v1`) first and `refs/notes/amont-gate` (header `amont-gate-v1`) only when the green ref has no note for the tree; Phase 4 (the plugin's check name `greengate-gate@<tree7>`) and Phase 5 (the census removing the old read) come on later branches.

## Code review
- relais: backend, lang:rust, lang:typescript (unix, tui, architect left out: two readers, no command-line change)

## Implementation review

- Phase 2 (relais reader), round 1 on tree bb7c1725: implementation-review approve · backend approve · lang:rust approve-with-changes · lang:typescript approve (the plugin parses `gate-stamp`'s JSON only, unchanged).
- Kept as deliberate (low): `NoteName`/`GREEN`/`LEGACY`/`READ_ORDER` stay `pub` (no caller outside `gate.rs`; relais is not consumed as a library; `LEGACY` goes at the census); `GateError` stays a string (a typed enum is a follow-up); no test pins the fail-closed path on a broken green ref (behaviour is correct and the message names the ref); no rename warning when the legacy ref is read (ADR-0068's notes sentence governs the window, and the plugin has a 1 s budget); `docs/router-protocol.md` moved forward with the reader (recorded in the canonical plan at Phase 2's close).

## Implementation review (Phase 4, writers switch)

- implementation-review, backend, lang:rust, lang:typescript: approve-with-changes, then approve (c3f05840). Fixed: the router check is `greengate-gate@<tree7>`; the docs, fixtures and CHANGELOG follow. A test that only counted its own fixture is deleted. The docs now say the check name is stored as written, and `kind = 'gate'` alone marks a gate check (no reader parses the prefix; the learning-loop count must count by `kind`).
- Deliberate: the prefix stays a literal at its one writer (router.ts).
- Correction for the canonical plan: there is no relais "ledger history reader" of `amont-gate@`, so Phase 5's grep needs no exception for one.
