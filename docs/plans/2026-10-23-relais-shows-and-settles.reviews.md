# relais shows its own work and settles what it can: full reviews

- **backend R1** (opus-5-5, 48k, 89 s): rework.
  - Blocking: findings reach `decide` through `Engine::stop`, which errors on a non-Stop `Next`.
  - High: `max_repairs` can be 2; the parser needs a finding definition; the retry needs a typed cause.
  - Medium: launching without the plugin; the reviewer told of widened tests; the pane bound; cost and success numbers.
  - All accepted.
- **typescript** (opus-5-5, 37k, 53 s): approve-with-changes. `AWAITS_A_PERSON` moves into timeline.ts; the bound includes quiet rows; a literal snapshot test; the successor launches from the outcome handler in runs.ts; paths quoted as data. All accepted.
- **react** (opus-5-5, 32k, 42 s): approve-with-changes. Trim order within a tall frame; quiet rows in the budget; only the newest awaiting run gets focus; the output share counts expanded frames only; a focused-Button test. All accepted.
- **ui-design** (opus-5-5, 28k, 34 s): approve-with-changes. Quiet rows; at least 8 output lines on small panes; a tall single frame; a `FAIL` label on folded rows; the artboard updated first; `isLast` clarified. All accepted.
- **ux-research** (opus-5-5, 29k, 54 s): approve-with-changes.
  - Accepted: a `↑ N rows above` cue (Baymard truncation); the live run kept over a newer awaiting run; the `MIN_ROWS` bound; failure text labels (WCAG 1.4.1).
  - Rejected (low, advisory): the limit of 3 folded runs is unbacked.
- **game-ux** (opus-5-5, 25k, 34 s): approve-with-changes. A `waiting for output` row for a silent step; an awaiting run never leaves the screen; `FAIL` on folded failures. All accepted.
- **backend R2** (opus-5-5, 56k, 24 s): approve-with-changes. All 7 round-1 findings resolved. Medium 8 (one trim order) and 9 (`review_repairs_used` on `Budget`, plus tests) carried into implementation.
- **codex** (gpt-6-astra, high): approve-with-changes.
  - High, applied: classify how the reviewer ended before retrying, and exclude cancellation and exhausted budgets; immutable per-dispatch review evidence.
  - Medium, carried: an automatic widen must not queue a parent decision prompt.
- **backend final** (opus-5-5, 58k, 12 s): approve. The two Codex edits hold.
- **Delta re-binds on the final body** (the hook counts fresh launches only): backend approve (28k), typescript approve (27k), react approve (20k), ui-design approve (20k), ux-research approve (22k), game-ux approve (19k). Low notes carried: trim order, focus after a fold, an explicit `< 29` bar.
