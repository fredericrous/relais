+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = ["t03-adjustment-shape", "t04-activations-ledger"]
repos = ["relais"]
+++
# The decision record for learned routing

## Question

Which decision does the relais ADR for Phase 8 record: that the router's
learned adjustments are served inside the envelope the person grants at
install, under the four gates, with automatic rollback (an extension of
ADR-0001 and ADR-0003), and whether SPEC §30 and §16 are amended in the
same record. The next free ADR number is checked at that time (0015 is
referenced by the learning-loop plan at line 36).

## Look at

- `.adr/records/0001-*.md`, `0003-*.md`, `docs/SPEC.md` §16, §17, §30, the
  Resolutions of the two tickets this one waits on.

## Answer looks like

The ADR's key, choice and reason, agreed by the person, written in the
same PR as the first serving plan.
