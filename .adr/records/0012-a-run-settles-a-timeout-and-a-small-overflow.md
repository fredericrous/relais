---
id: ADR-0012
status: accepted
decisions:
  - key: execution.settle-before-asking
    choice: a run settles five stops itself before asking a person - review findings all tagged `medium` or `low` send a `change` run back to its worker once (`review_repair`); a scope overflow made only of test paths outside protected prefixes, or of test paths plus at most `widen_scope_small_max_files` (default 1) non-test file of at most `widen_scope_small_max_lines` (default 10) changed lines, none protected and none a verification input, is answered by relais as `revise` with the scope widened (actor `relais`, codes `scope_widened_for_tests` / `scope_widened_small`); a reviewer that ended with no terminal result is dispatched once more when it was not cancelled or out of budget; and a `change` worker that hit its wall time with a confirmed stop and a changed tree has that tree published as the attempt's candidate and answered by relais with a `kind - verify` successor (code `timed_out_verify_queued`, `[execution] verify_timed_out`, default true); every automatic successor is reviewed at least `Required`, a successor of a relais settle is never settled again, everything else stops as before, and each settle is recorded in the ledger
    replaces: [ADR-0011]
    reason: in the 30 days to 2026-11-09, 57 runs ended `interrupted (process_crash)` at the worker's wall time, 39 of them salvaged by the orchestrating session at Opus prices ("the worker wrote the whole change; verified by hand"), and 30 of 88 `scope_exceeded` stops were one file of 10 lines or fewer; both next steps are mechanical, as ADR-0011's three were; the person chose a `verify` successor and the 1-file / 10-line limit (2026-11-10)
---
# 0012 — a run settles a timed-out tree and a small scope overflow

## Context

ADR-0011 moved three mechanical stops off the orchestrating session. Two more cost the most after it, measured from the ledger over the 30 days to 2026-11-09:

- **Wall time.** A worker that hit its attempt's wall time ended `interrupted (process_crash)` with no candidate: the tree was held, never snapshotted, so only the orchestrator could go on. 57 runs ended that way and 39 were salvaged by hand, most of them a finished change cut off before the worker reported.
- **A small overflow.** Of 88 `scope_exceeded` stops, the test-only ones were already widened (ADR-0011). Of the rest, 30 touched one file with 10 or fewer changed lines: a field the change needs, a line in an example.

## Decision

1. **A timed-out worker's tree becomes a candidate when its stop is confirmed.** At wall time relais asks the worker to stop and waits for the acknowledgement: the plugin's stopped state, within the spawn grace. Workers are native; no subprocess worker exists to confirm. With a confirmed stop and a tree that changed from its floor, the tree is snapshotted: artifact files first, written atomically, then the attempt's candidate and its patch evidence in one ledger transaction. An unconfirmed stop, no edit or an unresolved conflict ends as before, with no candidate.
2. **relais answers that run with a verify.** A `change` run that ended so is answered by relais (actor `relais`, `revise`): a `kind: verify` successor of the candidate, scope unchanged. The run ends `cancelled (timed_out_verify_queued)` and its worktree is retired. `[execution] verify_timed_out = false` restores the stop.
3. **A small source overflow is widened like a test-only one.** Test paths stay unlimited; at most `widen_scope_small_max_files` non-test files of at most `widen_scope_small_max_lines` changed lines (added plus removed) may join them, none under a protected prefix, none a verification input, every count known. Code `scope_widened_small`; `widen_scope_small_max_files = 0` turns it off.
4. **Every automatic successor is reviewed, and never settled again.** Its contract's review is raised to at least `Required`, a stronger one kept. A run launched from a relais settle's intent is not settled by any of these rules.

## Consequences

- ADR-0010 still holds: a wall-time end is still evidence recorded as a decision. relais answers it with a verify of the tree that exists, never a repair or a new worker dispatch.
- A settled run whose verify successor fails ends `failed` with no open decision; a person still finishes it with `decide --answer salvaged`.
- Once an interrupted attempt carries a candidate, any `--candidate <run>` without an index resolves to it.
- A timed-out run with a candidate exits as cancelled (7), not interrupted.
- `relais report` counts each settle and how its successor ended (`auto_settled`), from the successor intent rows, never from note text.
- Check 14 days after each release: orchestrator salvages of timed-out runs per week (from about 9.1), orchestrator decisions on small non-test overflows (from about 7), and the successors' outcomes. Roll a rule back by its key, never by an older binary.
