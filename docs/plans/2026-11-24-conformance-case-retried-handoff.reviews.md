# Full reviews: Conformance case for a retried handoff

Plan: [2026-11-24-conformance-case-retried-handoff.md](2026-11-24-conformance-case-retried-handoff.md). All seats ran on opus.

## Round 1 (body e22026180d11)

- architect — approve-with-changes (28k, 24 s). 1 low: CHANGELOG names relais-fabric as the consumer, no release cut (accepted). 2 low: name the array length 11 → 12 (accepted). 3 low: `run_all_with_clock` is the home of a later expiry case (accepted).
- backend — approve-with-changes (40k, 55 s). 1 medium: step 5 accepts `NodeDecides`, so a primary that never fences passes; add a strict primary-only check in `run_all_with_clock` (accepted). 2 medium: no relais-fabric run (accepted: owed at its pin bump, it is not on this machine). 3 low: one fixed `now` (accepted). 4 low: "writes the missing claim file" (accepted). 5 low: Phase 2 files the inbox entry (accepted).
- lang:rust — approve-with-changes (49k, 72 s). 1 medium: assert `holds()` maps t1 to `Session { me, Handoff }`, one `now` (accepted). 2 medium: a mutation row for step 5 (accepted). 3 low: hold wording matches the fake (accepted).
- po — approve-with-changes (27k, 56 s). 1 medium: Outcome names the owed check and rollback trigger (accepted). 2 medium: Phase 2 files the give-back path (accepted). 3 low: the case holds the end result, not `Held` (accepted, by hand).
- tui — approve-with-changes (27k, 22 s). 1 medium: step messages state the rule, name the step and print the answer (accepted).
- unix — approve-with-changes (24k, 20 s). 1 low: the Phase 2 check names its directory, unpiped (accepted).

## Round 2 (body 3cf65e4db948)

- architect — approve (30k, 17 s). 1 low: say "the age a `Held` answer reports" (carried into implementation).
- backend — approve-with-changes (31k, 24 s). Round 1 all resolved. 1 low: Goal says one case (carried). 2 low: name the inbox file (rejected: the relais inbox is `<git-common-dir>/greenharness/inbox.md`, never committed by design; the Resolution quotes the line).
- lang:rust — approve-with-changes (35k, 44 s). 4 high: the step-5 mutation (drop the early `Held`) would survive, since `try_acquire` still refuses `other`; clear `row.holder` on a handoff row instead (applied). 5 medium: Phase 1 names the primary case and the `run_all_with_clock` doc (carried, implemented). 6 low: match `Held { holder, .. }` (carried, implemented).
- po — approve-with-changes (32k, 28 s). 1 high: the Outcome must name `run_all_with_clock` and both checks (applied). 2 low: the map line records the owed relais-fabric run (carried).
- tui — approve-with-changes (27k, 19 s). 1 low: messages start with the check name, since both cases number steps 1, 2, 5 (carried, implemented).
- unix — approve (30k, 18 s). 2 low: name the exit-code table for a failing check (rejected: the check is an exit-0 gate on a markdown change; its failure codes are not this plan's).

## Backend on the final body (c36623a9e794)

- backend — approve-with-changes (48k, 84 s). po#1 and lang:rust#4 resolved. 1 medium: the owed relais-fabric check needs an owner and date (carried: recorded in the homelab Resolution and the map line). 2 low: Goal "one case" (carried). 3 low: the Picture labels the LocalState branch loosely (carried). 4 low: the Resolution quotes the inbox line in full (carried). 5 low: SlugLock ordering is asserted, not exercised concurrently (carried). 6 low: state `HANDOFF_HOLD_SECS`'s value and cost (carried).

codex: not run (programme ticket agent).
