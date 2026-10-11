---
id: ADR-0003
status: accepted
decisions:
  - key: learning.automatic-promotion
    choice: relais promotes a task artifact that passed every evaluation gate on its own, inside a `[learning]` envelope recorded at install, rolls the active artifact back on evidence, and does not promote the same data twice
    first: true
    reason: A loop that stops at "awaiting promotion" never changes how relais routes, so the three weeks of evidence in the ledger taught it nothing; SPEC §17 already allows automatic promotion inside an explicitly authorized envelope with independent gates, and automatic rollback with a refusal to repeat what was rolled back is what makes that envelope safe to grant once
---
# 0003 — relais promotes a passing task artifact inside an envelope

## Context

The learning loop was written whole (SPEC §16–§17): dataset, train,
evaluate, promote. After three weeks nothing had run it. `features` held
241 rows, no dataset had ever been built, the registry was empty, and no
dispatch was `routed_by = learned_artifact` (plan
`docs/plans/2026-10-10-learning-loop.md`). Nothing scheduled the steps, and
`relais promote` is a command only a person types, so even a cycle that
trained a passing artifact would have waited for a person who has no reason
to look.

SPEC §17 says "scheduled retraining and automatic promotion are permitted
only within an explicitly authorized envelope with independent evaluation
gates". ADR-0001 built that envelope for the session router: granted once,
at install, recorded with its source, revoked by `off`. The task artifact
(`relais run` routing) had no such envelope.

Every session runs with permissions bypassed, so a promotion that needs a
prompt would not be protected by one; what protects the person is that the
promotion path is narrow, the evidence is recomputed, and the way back is
immediate.

## Decision

A learning cycle (`relais learn cycle`, SPEC §17) promotes a task artifact
that passed every gate, with no person in the loop, when all of these hold:

- **An envelope was granted.** `[learning] auto_promote = { granted_at, by,
  source }` in machine.toml. The plugin install (`relais install --claude
  --write`) records it (source `install`; installing is the authorization)
  unless an `off` tombstone is there; `/relais-learning on|off` records or
  removes it through `relais native learning-envelope`, with the locked,
  comment-keeping writer the routing envelope uses. `off` also writes the
  tombstone, so a later install does not record it again. Without the
  envelope the cycle stops at `awaiting_promotion` and prints the `relais
  promote` line. The plugin refuses the model's own writes of it.
- **Nothing bypasses `evaluate`.** The artifact goes through
  `Registry::evaluated`, which recomputes every gate from the stored report,
  and then `Registry::promote`. A cycle never edits a threshold, a floor or
  a risk rule.
- **Rollback is automatic and manual.** Each cycle first checks the active
  artifact against the runs it routed. It rolls back on any `reverted` or
  `confirmed_regression` outcome of such a run, or, with at least 10
  labelled runs, when the one-sided 95% Clopper-Pearson upper bound of their
  acceptance is below the quality floor the artifact's own evaluation used.
  `relais rollback` does the same by hand. Both write a `learning_cycles`
  row, with source `cycle` or `person`, and the report and the pane name it.
- **Anti-flap.** A cycle does not promote an artifact whose dataset
  fingerprint and label policy match a rolled-back one, or the active one,
  and promotes nothing until 10 root runs have reached a terminal state
  since the last rollback. Retraining on the same evidence that just failed
  would only produce the same artifact.
- **One cycle at a time, and only when idle.** The cycle holds the one
  Training lease, refused (exit 20) rather than queued, and defers while
  another session's agents run (`training_when_idle`).

## The stated limit

The gates measure the acceptance model on held-out data from this
machine's own ledger. They do not prove that routing by the artifact is
better than routing without it, which only the routed runs show later. That
is the reason rollback exists and is automatic, and it has a cost: the
first runs an artifact routes are the evidence against it. The bound needs
10 labelled runs, so a quietly bad artifact can route up to 9 runs before it
is rolled back on acceptance alone. A single revert or confirmed regression
rolls it back at once.

With bypass permissions the model can edit machine.toml. The plugin's
guards refuse that, which stops an accidental self-grant and not a
deliberate one; the same accepted limit as ADR-0001 and SPEC §5. Every
envelope write is recorded with its source.

## Consequences

- SPEC §17 documents the cycle, the lease, the envelope, both rollbacks and
  anti-flap; §11 documents the report's learning section.
- The ledger gains `learning_cycles` (schema 22), an additive migration.
- `relais learn cycle`, `learn last` and `rollback` are new commands, and
  exit code 20 is new.
- `[learning]` joins machine.toml. An older relais refuses the table
  (`deny_unknown_fields`), so relais is upgraded before the table appears.
- A model that is not a task artifact (the session router's learned
  adjustments, a Laya classifier) is outside this decision; each needs its
  own plan, its gates and its record.
