+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
+++
# What a relais review finding holds that a sibling could use

## Question

What does a review finding look like on disk today, and which of its parts
(severity, rule id, `file:line`, statement, disposition after the repair)
can be read by code without a model, so a finding can be indexed by the
files it is about and its outcome known?

## Look at

- `crates/relais/src/review_outcome.rs`, `crates/relais/src/runner/machine.rs`
  (findings to the worker once, severities), the reviewer's output contract
  in `claude-plugin/agents/` and the `FINDINGS:` / `Candidates:` blocks in
  SPEC §7.
- Ten recent review result files from the ledger's `review_parts.result_path`.
- `rulings.jsonl` (`adr-lint.ruling/1`).

## Answer looks like

The fields a finding reliably carries, with the share of recent findings
that carry each; whether the repair's outcome (fixed, kept, overruled) is
recorded anywhere; and the smallest change to the reviewer contract that
would make the missing parts reliable.
