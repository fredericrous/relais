---
canonical: amont:docs/plans/2026-11-26-held-marker-names-live-lease.md
phases: [1, 2, 3]
status: active
---
Part of [Held marker names a live lease](https://github.com/greenharness/greengate/blob/main/docs/plans/2026-11-26-held-marker-names-live-lease.md)
(amont programme host-slot-admission, ticket t15). This repository carries
relais's slice of each phase: phase 1 reads the marker through the slots
crate's reader, phase 2 writes `held:<lease>`, phase 3 links the crate that
refuses the bare `held`.

## Code review

- relais: backend, lang:rust, unix, architect (tui left out: one stderr line, no table or prompt)

## Decision log

- 2026-11-27: the requirement floor moves to `"5.4"` in phase 1, not phase 3:
  phase 1 calls the reader (`held_marker`, `MarkerLog`) that 5.4 first
  publishes, so `"5"` would admit a crate it cannot build against. Phase 3
  raises it to the release that refuses the bare marker.
- 2026-11-27: the isolated-run golden gains one `host_slots_off` environment
  warning per scenario, and `outcome_after_session_line` skips that one
  stderr line: the suite now runs with host slots off
  (`GREENGATE_HOST_SLOTS=0`) instead of a bare `held` marker, and relais says
  so once per run. Nothing else in the golden moved. Regenerated as the
  test's rule says: at origin/main `6a99e144d` with only this branch's
  `.cargo/config.toml`, `RELAIS_GOLDEN_GENERATE_AT_COMMIT=1` wrote a golden
  byte-identical to the branch's (`cmp`).
- 2026-11-28: rebased on origin/main `82958ca93` (CHANGELOG conflict only;
  the golden still passes unchanged). On the final tree, through
  `greengate slot run` on the pinned 1.94.1 toolchain (`--now` after 50 min
  at the head of a line held at load 240-470, its `now` row at
  `at_ms` 1795822181561, pid 97383): `cargo clippy --workspace --all-targets
  -D warnings` clean, `cargo test --workspace --no-fail-fast` 3,336 passed,
  0 failed, 5 ignored. The real `/tmp/greenharness-slots-501/waits.jsonl`
  gained 28 rows from that row to the suite's end, all `greengate` and
  `greengate slot run` (`admitted` 10, `released` 8, `timeout` 5, `deferred`
  4, `now` 1) from other sessions' jobs: no `"tool":"relais"` row and no
  `marker_refused` or `marker_bare` row. (The byte offset the script meant to
  take first was lost to GNU `stat`; the window is read by `at_ms`.)
- 2026-11-28: implementation review moved the marker out of process
  statics: `verify::HostMarker` (the judged marker and its `MarkerLog`) is
  read once in `cli/run.rs` and `cli/replay.rs` and carried on
  `RunConfig::host_marker`; a child run uses its parent's. The refusal is
  also an `EnvironmentWarning` `host_marker_refused`, as `host_slots_off`
  is.
- 2026-11-28: rebased again on origin/main `28a2f9b23` (relais v0.93.0,
  #641's paced wait). The requirement floor is now main's `"5.5"`, which
  carries 5.4's reader, and the lock is main's (5.5.0); the CHANGELOG entry
  sits under `## Unreleased`. All Rust auto-merged; clippy `-p relais
  --all-targets -D warnings` clean; all five reviewers approved the rebased
  tree (`bffe16e1…`) as a delta.
- 2026-11-28 (phase 2): relais writes `held:<lease>` for its own lease and
  the kept slot's, and passes an accepted inherited marker through
  unchanged. `HostMarker` reads `GREENHARNESS_HOST_SLOT` once (`var_os`) and
  judges that same text with `held_marker_in(&dir(), …)`, keeping it only
  when held; `judge` returns a named `Judged`. Two files beyond the phase's
  list follow from it: `runner/engine.rs` (the `Judged` destructure) and
  `docs/SPEC.md` (what the check's environment gets). The lock stays on
  crate 5.5. Observed on the round-1 tree, pinned 1.94.1, through
  `greengate slot run`: clippy `--workspace --all-targets -D warnings`
  clean, `cargo test --workspace --no-fail-fast` 3,339 passed, 0 failed,
  5 ignored; the real `waits.jsonl` gained 12 rows during the suite, none
  `"tool":"relais"` and none `marker_*`. After the review fixes: clippy
  `-p relais` clean, `verify::` 183 and `runner::` 577 passed.

## Implementation review

- implementation-review: approve (rebase delta) after approve-with-changes in round 1 and two Deltas; fixed: log at the edge, then injected (`HostMarker`), suite counts recorded; deliberate: no runner test of the event (slots forced off suite-wide; phase 3 checks it), `given`'s doc (phase 2 tests it). 132k.
- backend: approve (delta); runner-boundary test deliberate as above; docs and once-per-run CHANGELOG fixed. 91k.
- lang:rust: approve (delta); `match` for the journal write; tuple invariant documented. 87k.
- unix: approve (delta); refusal is also an event. 87k.
- architect: approve (delta 2); golden kept as generated (deliberate); SPEC's "the check gets the marker" becomes true in phase 2 (deliberate). 114k.
- phase 2, implementation-review: approve (delta) after approve-with-changes; fixed: `inherited` name, no placeholder marker, extra files logged. 89k.
- phase 2, backend: approve (delta); fixed: one read of the variable, install order in the CHANGELOG. 84k.
- phase 2, lang:rust: approve-with-changes (delta); fixed: one read, `Judged`, `Option` marker; deliberate: `absent()`'s empty path, never read (the crate returns `Absent` for no value first). 86k.
- phase 2, unix: approve (delta); fixed: install order in the CHANGELOG. 78k.
- phase 2, architect: approve (delta); fixed: one read, private `judged`, no placeholder. 91k.
