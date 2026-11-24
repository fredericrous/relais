# Full reviews: A pin row records the model the call ran

Panel on Opus 5.5 (all seats), two rounds, then Codex.

## Round 1 (body 5ff374bdd763, 1421 words)

- **backend** (approve-with-changes, 32k, 38 s)
  - 1 medium, remove: unknown-preference validation does not serve the goal, since a preferred pin's row records the session model. Accepted.
  - 2 medium: the row's model is inferred; name nested-spawn and `/model` limits. Accepted.
  - 3 low: cases for rule 4's fallback and unknown `pin_sources`. Accepted.
  - 4 low: measure the fallback-to-pin-string rate. Accepted.
  - 5 low: merge ruling, phases stay apart. No edit.
- **unix** (approve-with-changes, 34k, 43 s)
  - 1 high: `router-state` has no `--cwd`. Accepted.
  - 2 high: the pilot is not isolated (real ledger migrated, user agents read). Accepted.
  - 3 low: the warning is invisible in sessions. Superseded by backend 1.
- **tui** (approve-with-changes, 30k, 50 s)
  - 1, 2 high: same as unix 1, 2. Accepted, merged.
  - 3 medium, 4 low: warning visibility and text. Superseded by backend 1.
- **lang:typescript** (approve-with-changes, 47k, 62 s)
  - 1 high: the session-model read must not throw or stall before `next(e)`. Accepted.
  - 2 medium: run the cases end to end through `routedSession`. Accepted.
  - 3 low: an unresolvable call `model` falls through. Accepted.
  - 4 low: the f20 overlap on `routeSpawn` lines 389-404. Carried as a note.
- **lang:rust** (approve-with-changes, 48k, 99 s)
  - 1 high: the pilot command (same as unix 1, 2). Accepted, merged.
  - 2, 3, 5, 6: alias list, regex dependency, own-beside-bad-preference, warning type. Superseded by backend 1.
  - 4 medium: test the serialized `pin_sources` values. Accepted.

## Round 2 (body 8b7cf276fbc9, 1549 words)

- **backend** (approve-with-changes, 36k, 23 s): round-1 findings resolved.
  - Low: "the typo cannot reach the row" holds only while a session model resolves.
  - Low: the live count should expect 0 only where the session model was known; fold the two Live bullets.
  - Low: the rejecting-session case should name a `preferred` pin.
- **lang:rust** (approve-with-changes, 51k, 14 s): resolved.
  - Low: add an own `opsu` case (row keeps `opsu`, tier `implementation`).
  - Low: stderr-empty relies on a missing ledger (true).
  - Low: a long line in the Phase 1 text.
- **lang:typescript** (approve-with-changes, 52k, 20 s): resolved.
  - Low: add preferred `opsu` + session haiku → `claude-haiku-5-5`.
  - Low: the live-count expectation; the guard wording covers only the session-model read.
- **tui** (approve, 34k, 10 s): resolved. Low: the duplicate Live bullets.
- **unix** (approve, 38k, 13 s): resolved. Low: the same duplicate bullets.

All round-2 findings are low, so they are carried into implementation:
the added test cases go into Phase 2, and the live-check wording goes into
the filed follow-up ticket.

## Codex final review (body 8b7cf276fbc9)

- **codex** (approve-with-changes, gpt via codex-cli 0.169.0, high effort)
  - 1 high: the session-model read had no deadline; a never-settling promise stalls `next(e)`. Accepted, applied (bounded with `within`/`waitFor`).
  - 2 medium: `resolveModel` maps an alias through a configured tier's version, which a pinned call never receives. Carried into implementation: the row resolves aliases with `DEFAULT_MODEL_IDS` and keeps full ids.

## Delta on the Codex edit (body 3298939a5d52)

- **backend** approve (39k, 19 s). Carried: read the session model in `afterSpawn`, after `next(e)`, so the spawn never waits.
- **lang:typescript** approve (56k, 21 s). Carried: map `TIMEOUT` to `undefined` before `resolveModel`.
- **tui** approve (35k, 5 s). **unix** approve (40k, 7 s).
