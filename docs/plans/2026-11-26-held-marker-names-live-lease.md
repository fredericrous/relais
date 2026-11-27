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
- 2026-11-27: the suite leaves no row in the real `waits.jsonl`: after a
  `cargo test -p relais` run (2,776 + 364 + 98 + … passed, 0 failed), the
  only `relais` row since its start came from another live session.
