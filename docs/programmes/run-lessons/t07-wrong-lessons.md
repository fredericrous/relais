+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais", "greenbelay", "decisions"]
+++
# How a wrong lesson is retired and a lasting one promoted

## Question

A lesson can be wrong (a reviewer's false finding, a workaround that only
hid a bug). What retires one, and what tells a lasting lesson apart, to be
promoted out of relais into an aval rule or a greenbelay corpus rule? What
do aval, greenbelay's graduate step and relais's recipe promotion (SPEC
§27) already offer that this can reuse?

## Look at

- relais SPEC §18 (aval outranks remembered claims), §27 (recipe promotion
  and rollback), `crates/relais/src/learn/revert.rs`.
- greenbelay `crates/greenbelay-guard/src/graduate.rs` and `docs/rules.md`
  (advise → deny on a corpus).
- How a decisions-corpus constraint is proposed (decisions `docs/`).

## Answer looks like

The retirement paths (explicit `relais lesson retire`, contradicted by a
later run, a review that overrules it), the promotion signal (repeat count,
age, distinct sessions) with what it reuses, and which tool owns the
promoted rule under `harness.tool-boundaries`.

## Resolution

Researched 2026-11-19 (Explore agent, read-only; `harness.tool-boundaries`
resolved active in the decisions corpus, ADR-0075 superseding ADR-0064's
form).

- **Retirement, three paths, all new code reusing existing parts:**
  - `relais lesson retire <id> --reason`, modelled on `relais recipe
    rollback` (SPEC §27): human-gated, append-only, a tombstone kept with
    its reason, never deleted;
  - contradiction by outcomes: a workaround's source run later `reverted`
    (`learn/revert.rs`) or `corrected` (SPEC §20), or siblings that were
    given the lesson and still fail the same way, judged at checkpoints
    like `learn::typed_rollback`; the learn cycle only notes a candidate,
    a person or a nonce-carrying feedback row retires it;
  - a finding judged false by `relais dataset judge-trial` ground truth is
    retired at once; a lesson that conflicts with an active aval decision
    is dropped without a vote (§18).
- **Promotion signal** (none exists to reuse; design): distinct sessions,
  not runs, that hit it; an age of at least `REVERT_WITHIN_DAYS`; not
  retired and evidence still valid (t04). Following greenbelay's
  `graduate` (`graduate.rs:1-47`): gate on reviewed cases, never on rate;
  demotion is free, promotion needs evidence; mined lessons are only
  candidates, the rule is written by hand.
- **Owner of a promoted rule** under `harness.tool-boundaries`: an agent
  tool-use mistake becomes a greenbelay corpus rule; a mechanically checked
  code convention an ADR-adopted greenspotter rule; a lasting decision a
  decisions ADR. relais only hands off the candidate with its evidence,
  writes no aval or greenbelay file, and retires its own lesson once the
  rule is adopted.
- Thresholds are new: [When a lesson is a promotion candidate](t09-promotion-thresholds.md).
