# Reviews: A session claims its ticket, and a handoff names only an unheld one

Round 1 (opus, about 32–46k tokens and 43–77 s each):
- backend, approve-with-changes: claims log (accepted); takeover race (accepted, merged into the slug lock); refresh (duplicate of lang:rust 3 and po 2); `--dir` always passed (accepted); phase files named (accepted); "release with grh" (rejected: it is the skills version release); test expectations (accepted).
- architect, approve-with-changes: park line keeps the `clear-at-boundary` marker (blocking, accepted); Phase 2 edits worktree-task, merge-when-green and tag-release (accepted); claims reach `analysis::standings` as values, `held_by` in JSON (accepted); ADR-0078 `proposed` with Phase 1 (accepted).
- po, approve-with-changes: claims log (duplicate); refresh at each commit (accepted); programme-level bars (accepted); rollback threshold (accepted).
- unix, approve-with-changes: exit 22 `Parked` (accepted); atomic create and takeover (accepted, as the lock); `resolve_session()` (accepted); refusals on stderr (accepted).
- tui, approve-with-changes: exit code (duplicate); streams (accepted; `parked:` stays on stdout as the answer); held mark after `cut_name`, held rows last (accepted); `claimed <s8> <age>` fifth field (accepted); `--session` (accepted).
- lang:rust, approve-with-changes: `procs::LockFile` for every write (accepted); Parked (duplicate); refresh (accepted); deterministic and barrier race tests (accepted); `sha2` repo key (accepted).

Round 2 (resumed, 17–38 s each). po and tui approve. backend, architect, unix and lang:rust approve-with-changes, with findings **carried into implementation**:
- backend: stale `create_new` wording in `next --claim` and the Picture; rollback must count `work` claims only and name the switch.
- po: compare the expiry share with Phase 0's no-`SessionEnd` share, not a fixed 10%.
- architect: `respond.rs` returns early for `resume`/`compact`, so the refresh must run before `fresh_session_cwd()` (200 ms lock wait, outside `GATHER_DEADLINE`); the `SessionEnd` delete goes in `follow_agent_lifecycle`.
- lang:rust: hooks try the lock once and never retry; a skipped `SessionEnd` writes `claims/ended/<session>`; a 100 ms `SessionStart` test; each test thread opens its own lock handle.
- unix and tui: a lock timeout exits 1, with stderr naming the `.lock` path and holder PID, then `next: <same command>`.

Codex (gpt, high effort), approve-with-changes, 3 high, all applied: step 2 re-checks the whole destination programme under its lock; automatic handoff skips tickets due later; refusal recovery turns the reservation into a `work` claim.

Backend delta after Codex, approve-with-changes, **carried**:
- release the `--from` lock before step 2, and count a timed-out destination lock as occupied;
- step 3's park date must point to the eligibility bullet (it contradicts it now);
- "select again" is at most 3 tries, releasing each lost reservation, then parks.

