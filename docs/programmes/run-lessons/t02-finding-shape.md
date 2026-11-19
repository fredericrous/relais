+++
type = "research"
mode = "afk"
status = "closed"
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

## Resolution

Researched 2026-11-19 (Explore agent, read-only; 31 findings in the 10
newest `runs/<run>/review-*.txt`, plus 7 in the 3 rows `review_parts`
holds).

- **On disk:** prose, then `FINDINGS:` and numbered lines `N. [sev]
  path:line — text`. Only the severity is parsed (`finding_severities`,
  `runner/review/mod.rs:73-91`), and a single untagged finding makes all
  severities unknown, so the review goes to the person.
- **Carried:** severity 31/31, statement 31/31, `path:line` about 24/31
  (absolute and relative mixed), a rule id about 22/31, a criterion label
  instead about 10/31. Split reviews are worse (severity 3/7).
- **Disposition: recorded nowhere.** The `review_repair` transition keeps
  the raw findings text (`machine.rs:747-780`); the re-review is a new file
  with no link to the old findings; `outcomes` is run-level only;
  `rulings.jsonl` holds the reviewer's own call on adr-lint candidates, not
  the repair's.
- **Smallest change:** (1) a fixed line grammar, `N. [sev]
  <rule-id|criterion:X|none> <repo-relative path>:<line> — statement`, in
  `report_severity_instruction` (`review/mod.rs:94`) and the reviewer agent
  text (`install/mod.rs:285-300`); (2) parse the whole line into typed
  findings on `Observation::ReviewFindings`; (3) a stable finding id (hash
  of rule, path and line) and a ledger `findings` table, the re-review
  marking each fixed (gone) or kept (still there). (1) and (2) are what a
  sibling needs; (3) is what t03 needs to pass only confirmed findings.
