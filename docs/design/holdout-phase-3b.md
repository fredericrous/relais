# Design: holdout overlay, protected inputs and what a worker sees (ADR-0045, phase 3b)

Phase 3a runs and settles holdouts. Phase 3b closes the ways a worker could
reach or bend one.

## 1. The overlay is reserved (attempt.rs, scheduler.rs scope checks)

Before the scope check, compute the candidate's changed paths from the run's
floor (the same `changed_paths_from` that `check_scope_from` uses). Any path
equal to a resolved holdout's `decl.overlay`, or under it when the overlay
is a directory, means the attempt is finished and the run ends:
- `Terminal::Failed`, with a new `Reason::HoldoutOverlayCollision` /
  `BlockCode`-free code `holdout_overlay_collision` (exit like any `failed`);
- no repair, no escalation, no `scope_exceeded`, no `revise --widen-scope`;
- the detail, as the person sees it, names the holdout and the overlay path.
  No prompt ever carries it: the run is over.

The same check runs in the scheduler's integration path (`scheduler.rs`, a
decomposed run) and on a `kind: verify`. Widening a scope never adds an
overlay path: `decide revise --widen-scope` and `widen_scope_for_tests`
filter overlay paths out. Since the collision ends the run first, that is a
second guard.

## 2. A holdout's own inputs are protected

`HoldoutDecl` gains `inputs: Vec<String>` (repo-relative globs, default
empty, `deny_unknown_fields` unchanged): the files the holdout command
depends on in the candidate's tree (for Rust, the crate's `Cargo.toml`, a
`.config/nextest.toml`). They join `verify::verification_inputs` as the
**policy** class for a run whose contract names that holdout, so a candidate
that changes one stops `needs_decision` (protected verification, SPEC §9),
like any profile input. Phase 5 counts how many runs this stops.

## 3. What a worker, the reviewer and a successor can see

Already true after 3a: the gap text holds only the criterion id, the holdout
name and the test ids. 3b proves it at every consumer and closes the rest:
- the repair prompt (`build_prompt` with `previous_failures`), the review-repair
  prompt, the escalation prompt and the reviewer's prompt hold none of the
  store, path, overlay, config, the script's lines or the kept log/report paths;
- the receipt section the reviewer is given lists each holdout as
  `name: outcome (test ids)` only;
- `read_hints` and a successor's context never name the run's artifacts
  `holdouts/` dir: a hint under the state dir is already refused, so assert it;
- the exhaustion classifier (`exhaustion.rs`) reads only profile checks, never a
  holdout log, so a holdout's output can never be quoted in a `resource_exhausted`
  detail: a test asserts it.

## 4. A run that runs out of attempts on a holdout

When the attempt ceiling is reached and every remaining failure is a holdout
gap (`HoldoutFailed` / `HoldoutDidNotBuild`), the run ends
`needs_decision (holdout_failed)` instead of `failed`. Its decision detail
names, for the person, each holdout and its sha, the failing test ids, the kept
log and report paths under the run's artifacts, and the answers:
`relais decide <run> --answer approve|revise|reject`. `approve` is refused, as
for any gap. Mixed failures end `failed`, as before.

## 5. Missing repository name (carried from phase 2)

When `main_checkout_name` finds no name and the contract names a holdout,
preflight refuses with a detail saying the repository name could not be
determined, instead of `holdout_unknown (declared: none)`. The code stays
`holdout_unknown`.

## Tests

- A worker that writes the overlay path ends `failed (holdout_overlay_collision)`
  with no repair dispatched, and no prompt artifact contains the overlay path.
- A worker that changes a declared holdout input stops `needs_decision`.
- With a failing holdout and repair allowed, the repair prompt, the escalation
  prompt and the reviewer prompt contain the statement and the test id and none
  of the store, path, overlay, config, a script line or the kept paths.
- Every attempt failing on the holdout ends `needs_decision (holdout_failed)`,
  and its detail names the kept log path.
- A missing repo name gives the new detail.
