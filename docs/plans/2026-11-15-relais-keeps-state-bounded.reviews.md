# Full reviews: relais keeps its state directory bounded

**Round 1** (all opus; tokens, wall time):
- architect — approve-with-changes (21k, 25 s): purge not monitored; decide the hook→procs lock now (one `fsutil::rotate_if_over`); guard against in-place writes to `candidate-*.patch`; show `RELAIS_AUTO_PRUNE_DAYS` in doctor. All accepted.
- rust — approve-with-changes (25k, 33 s): per-run pin re-check; `tree_bytes` dedupe per run dir; EXDEV coverage; future stamp mtime is stale; injected apply closure; fsync net. Accepted (3, 5 as implementation detail).
- tui — approve-with-changes (20k, 28 s): observable prune summary; manual line format; keep tally; invalid env fallback (rejected: a deleting job must not guess); doctor layout; exit code unchanged (rejected: `resume.rs:413` convention); summary after "no run worktree".
- unix — approve-with-changes (22k, 27 s): crash-safe `.pruning-<id>`; streams and exit; `--no-prune` > env > 30; `resume.log` rotated before `open()`; Windows rename; no dir fsync. All accepted.
- backend — approve-with-changes (30k, 74 s): purge monitor and early-return reach; numbers did not add up (measured: live verify checkouts); stale pins (the "ended_at" part rejected); picture; observable verification; code-review reason. All accepted.
- po — approve-with-changes (25k, 96 s): target, cutoff (30 d = 1.1 GiB), Phase 1 value (👉 decision), monitoring, rollback. Accepted except 3.

**Round 2** (resumed): rust approve-with-changes (35k, 16 s); architect approve-with-changes (29k, 14 s); unix approve (30k, 13 s); tui approve-with-changes (29k, 19 s); po approve-with-changes (33k, 26 s); backend approve-with-changes (47k, 49 s; re-runs 11 s and on the Codex edit). Only backend's high Context fix changed the body.

**Codex** — approve-with-changes: the prune subtree holds package checkouts (high, applied to judge check 4); journal chmod by path races a rotation; the first rotation keeps the 332 MB journal as `.1`.

**Carried into implementation (18):**
1. Move `LockFile` into `fsutil` (re-export from `procs`); `rotate_if_over` takes it (rust, backend).
2. Every `runs/` enumerator (judge, doctor, `tree_bytes`, listings) skips `.pruning-*`; test a planted one is finished, not judged (rust, architect, tui).
3. (dropped with Phase 1.)
4. The rename to `.pruning-<id>` is the commit point; a pin after it yields `candidate_missing` (architect).
5. Leave `daemon_log` behaviour unchanged, or test it identical (unix).
6. Doctor warns on the symptom (ended runs older than age + 3 d), with the stamp age as information, so a prune that fails every time is seen (tui, unix).
7. Verification: a second start adds no `prune`/`auto-prune` line (the `no run worktree` line still appears) (tui).
8. An invalid env value skips only the prune; the retirement still runs; doctor shows the value (tui).
9. (dropped with Phase 1.)
10. Tests: `--no-prune`, invalid env, `.pruning-*` recovery, pin added after the judge loaded, failed delete keeps the stamp and exits OperationalFailure (backend).
11. Name `runner/candidate.rs` and `fsutil.rs` in the phase headings (backend).
12. Journal: set the mode through the open fd; test open → rotate → chmod → append (codex).
13. The first rotation keeps today's 332 MB as `.1`; report it separately until the second rotation, and test an oversized journal through two rotations (codex).
14. (dropped with Phase 1.)
15. `auto_prune` takes an injected apply closure to assert the stamp is written first (rust).
16. No parent-dir fsync after a rename (unix).
17. List judge check 4's child-checkout integration test under Integration tests too (backend, on the Codex edit).
18. The doctor's purge finding also counts old `accepted` runs kept only for a missing receipt (118 of 318 since Oct 15), the one class still never pruned; waiving it too is the person's later call (backend, receipt delta).
