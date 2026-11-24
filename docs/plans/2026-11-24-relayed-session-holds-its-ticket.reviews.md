# Full reviews: A relayed session holds its ticket

Plan: [2026-11-24-relayed-session-holds-its-ticket.md](2026-11-24-relayed-session-holds-its-ticket.md).
Every seat ran on opus.

## Round 1 (body 20f91ff9b7c1, 1195 words)

- **backend** — approve-with-changes (37k, 51 s). 1 high: a failed adopt
  is silent, so 11-21 comes back (accepted: retry + pane note). 2 medium:
  per-ticket partial failure (accepted: skip lines, loop goes on); a
  production measure (accepted as a dated follow-up ticket, not a
  Verification row). 3 medium: non-observable Verification rows (accepted).
- **lang:rust** — approve-with-changes (43k, 68 s). 1 high: no way to move
  the clock in the CLI tests (accepted: `World::age_claims`). 2 high: the
  claim path cannot be reused as is (accepted: extract a non-printing
  core). 3 medium: `claim.repo` may be empty or a removed worktree
  (accepted: fallback to the primary checkout). 4 low: case 3 asserts
  silence (accepted).
- **lang:typescript** — approve-with-changes (34k, 42 s). 1 high: the
  test fakes count adopt as a read (accepted). 2 medium: no event variant
  (accepted). 3 medium: race on `memory.previous` (accepted: capture
  before await). 4 medium: older relais re-spawned (accepted). 5 low:
  `adopt` in `PROGRAMME_COMMAND` (accepted).
- **tui** — approve-with-changes (30k, 38 s). 1 high: `claim_command`'s
  hints and exit 3 would leak (accepted, merged with rust 2). 2 medium:
  held line without age (accepted). 3 medium: match `'adopt'`, not the
  `session` pattern (accepted). 4 low: usage messages name the flag
  (accepted).
- **unix** — approve-with-changes (26k, 21 s). 1 medium: no `next:`/
  `--steal` hints, empty stdout (accepted). 2 medium: partial-failure exit
  semantics (accepted). 3 low: shared deadline (accepted, merged with
  typescript 3). 4 low: idempotence test (accepted).

## Round 2 (body 3838bb5be3bf, 1504 words)

All round-1 findings resolved. New:

- **backend** (47k, 13 s): medium, retry ids held as `{old, new}` and
  cleared on a session change; low, retry inside the read's deadline.
  Carried into implementation.
- **lang:rust** (50k, 23 s): medium, a skip for a missing repository must
  not read as success; low, restore the full-gate row. Carried (the row
  was restored by typescript 6).
- **lang:typescript** (43k, 29 s): **high**, `pnpm -C claude-plugin test`
  cannot run, there is no package.json; the runner is `make plugin`
  (applied). Medium, retry ids (as backend); low, name the retry tests.
  Carried.
- **tui** (35k, 15 s): medium, the pane note must be a runnable command
  (the adopt retry with both ids). Carried. Low, gate row: no edit.
- **unix** (32k, 22 s): medium, clear the pending adopt on exit 2 and
  stop retrying after R. Carried.

## Backend on the final body (0f4eb3921e8b)

- **backend** (49k, 11 s) — approve-with-changes: the `make plugin` row
  resolved. Medium: `make plugin` can pass with `PLUGIN_SKIP_OK=1`, so
  expect no `SKIPPED` line; medium: the gate row should read `make check`.
  Both carried into implementation (Verification records the observed
  command).

## Carried into implementation (10)

1. `adoptPending = { session, previous }` as captured; cleared at
   `onProgrammeSessionEnd` and on a session change; retried only while
   `memory.session === adoptPending.session` (backend, typescript).
2. Exit 2 from `adopt` clears the pending adopt; retries stop R (2 h) after
   the clear (unix).
3. The pane note is the runnable retry, `relais programme adopt
   --previous <old> --session <new>` (tui).
4. A handoff skipped because its repository is not found exits 1, so the
   plugin keeps it pending and shows the note; a closed ticket's skip
   stays exit 0 (rust).
5. The retry shares the read's deadline; the delay is stated in the
   Decision log (backend).
6. Phase 3 tests name the retry, the retry after a resume, the note and
   the non-`adopt` exit 2 (typescript).
7. The plugin row expects no `SKIPPED` line (backend).
8. The gate row is `make check` (backend).
9. Check whether the shared-state grant has a lease the backdating misses
   (rust).
10. Measure a SIGKILL mid-adopt leaves `claims/` readable (unix).
