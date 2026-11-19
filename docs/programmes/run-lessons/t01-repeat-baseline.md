+++
type = "research"
mode = "afk"
status = "closed"
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

## Resolution

Measured 2026-11-19 (Explore agent, read-only) with
[`t01-repeat-baseline.py`](t01-repeat-baseline.py), rerun unchanged with
`python3 -I docs/programmes/run-lessons/t01-repeat-baseline.py -x`. A
repeat is within 14 days, same repository (`-wt-*` folded), an earlier
different run. `review_parts` holds only 3 rows, so findings come from the
run directories' `review-*.txt`.

| | All repos | relais | greenbelay (then amont-agent) |
|---|---|---|---|
| Runs / reviewed | 1,673 / 360 | 633 / 122 | 250 / 65 |
| Reviewer findings / repeats (shared file + rule id or ≥0.5 word overlap) | 162 / 1 | 84 / 1 | 59 / 0 |
| adr-lint `violation` rulings / same rule+pattern repeats | 162 / 89 | 33 / 26 | 24 / 18 |
| Failed checks / of a repaired sibling, same check / and same first error line | 211 / 137 / 5 | 99 / 78 / 0 | 32 / 27 / 0 |

- **Reading:** reviewer prose rarely repeats on the same files, but the
  same code rule is violated again by a sibling more than half the time
  (`names.reveal-intent/rust-weak-name` in 6 relais runs 10-26 to 11-07;
  `errors.never-swallowed/rust-let-discard` in 6 runs 10-27 to 10-31;
  `functions.no-flag-arguments/rust-bool-param` in 5). That is where the
  findings channel pays first.
- **Failures:** "same check" counts a command, not an error, so 137 is
  inflated; the error-line match (5) is crude, and one of its matches is
  a Node deprecation warning. A real workaround count needs the recorded
  workarounds the programme builds; this baseline is a floor.
- **Caveats:** finding extraction is heuristic (t02: no fixed grammar);
  "repaired" means the run ended accepted; 1,315 of 1,673 runs still have
  artifacts. The after-count must rerun this script on the same window
  length.
