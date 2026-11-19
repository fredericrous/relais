+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
+++
# How often a sibling run repeats a finding or a failure today

## Question

Across relais runs in the same repository, how often does a later run get a
review finding an earlier run already got (same rule id or same kind of
finding, overlapping files), and how often does a later run fail a check the
same way an earlier run failed and then repaired?

## Look at

- relais ledger `~/.local/state/relais/ledger.sqlite`: `runs` (repo,
  contract, started), `review_parts` (`result_path`), `attempts`,
  `transitions`, `check_timings`.
- Each run's artifact directory: review result files, `rulings.jsonl`,
  failure evidence of attempts that were repaired.
- Since 2026-10-10, worktrees folded into their repository
  (`<repo>-wt-*` → `<repo>`).

## Answer looks like

Per repository: runs, reviewed runs, findings, findings that repeat an
earlier sibling's within 14 days, and repaired failures that repeat an
earlier sibling's; a few named examples of each; and the query or script
that produced it, so the destination's after-count reruns it unchanged.
