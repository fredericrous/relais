+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais", "amont-agent", "decisions"]
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
