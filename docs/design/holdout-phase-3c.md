# Design: a successor is judged by its predecessor's holdout (ADR-0045, phase 3c)

`relais resume` never re-dispatches. A run continues only through a successor
intent (`decide revise --widen-scope`, `--successor-contract`, `review-parts`,
`--rerun-verify`). Today a successor resolves its holdouts again from
machine.toml and `ls-remote`, so an edit to machine.toml, or a store whose
`ref` moved, silently changes the evidence that judges the same work.
Phase 3c freezes the definition.

## Store the resolved definitions

A ledger migration (schema 43, expand-only) adds the table

```sql
CREATE TABLE holdout_definitions (
  run_id TEXT NOT NULL REFERENCES runs(id),
  name TEXT NOT NULL,
  definition_json TEXT NOT NULL,   -- serde of ResolvedHoldout (repo, name, store, sha, decl)
  digest TEXT NOT NULL,
  PRIMARY KEY (run_id, name)
);
```

Preflight writes one row per resolved holdout, in the same transaction as the
manifest evidence where one exists, else right after `resolve_all`.
`ResolvedHoldout` and `HoldoutDecl` derive `Serialize`/`Deserialize`.
The rows live in the ledger, never in a worktree or a prompt.

## A successor inherits

In preflight, when `ledger.intent_of_run(run)` names a predecessor:
- for each holdout name the contract's criteria use, read the predecessor's
  row. When it exists, use that `ResolvedHoldout` as is: no machine.toml
  lookup, no `ls-remote`. Write it again under the successor's run id, with
  the same digest;
- when the predecessor has no row for that name (a new criterion), resolve it
  as phase 2 does;
- when machine.toml now resolves that name to a different digest, emit a run
  event `holdout_definition_changed {name, frozen_digest, current_digest}`
  and keep the frozen one. A store that no longer has the frozen sha is caught
  at fetch, as `blocked (holdout_unfetchable)`, never by switching.

The receipt's `holdouts[].digest` already carries the digest, so a
successor's receipt shows the predecessor's digest.

## Carried from the 3b review

- A test shows that a `kind: verify` whose candidate wrote the overlay path
  ends `failed (holdout_overlay_collision)`. A verify runs no worker, so the
  check runs on the applied candidate.
- The scheduler's collision path (`runner/scheduler.rs`, decomposed runs)
  finishes the open attempt row as `failed` before it ends the run, as the
  attempt path does, so no attempt row is left open. A test reads the attempt
  rows after a decomposed run's collision.

## SPEC

§10 states the freeze, the `holdout_definition_changed` event, and that
`resume` never re-runs a holdout.

## Tests

- Predecessor and successor: run 1 resolves holdout `x` at store sha A. The
  test then edits machine.toml (a new `command` and `overlay`) and moves the
  store's ref to sha B. A `--rerun-verify` (or `--widen-scope`) successor's
  receipt shows digest and sha A, and the events hold
  `holdout_definition_changed`.
- A successor whose contract adds a new holdout name resolves that one fresh.
- The migration: a schema-42 ledger opens and upgrades; the table exists and is empty.
- verify collision, and the scheduler attempt rows (above).
