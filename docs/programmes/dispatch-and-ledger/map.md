# relais checks once and reviews nowhere twice

## Destination

relais keeps acceptance as one interface with two adapters: through
greengate (`amont run --stamp`) when amont is present, so the push gate
reuses that verdict instead of running the checks again, and a plain command
run when it is not. relais's own semantic reviewer is removed in favour of
implementation-review, and a baseline runs only when greengate cannot prove
the base. Decided, and running on relais and amont, with check-hours and
review spend per run measured before (t02) and after.

## Notes

- **Origin:** session of 2026-11-10. On amont since 2026-10-10, baselines took
  9.1 h (0 cache hits out of 16) against 3.6 h for every attempt's
  verification combined (`check_timings`); fleet-wide, review is 38% of
  relais's spend (t02). The amont pre-push gate and the
  worktree-task verification loop then check the same tree again, and
  implementation-review (F4b) reviews the diff that relais's reviewer already
  read.
- **The person's choice (2026-11-10):** "Delegate, stay standalone". relais
  stays usable without amont ("useful alone, better together"): its
  acceptance is what sets it apart from a model picker, and a hard
  dependency would make greengate a requirement for trying relais. An
  earlier choice, a hard removal with relais depending on amont, was
  withdrawn the same day.
- **Governing decisions:** resolve before deciding a ticket:
  `harness.tool-boundaries` (decisions ADR-0064: greengate owns gates and
  stamps, greenlift reaches them only through greengate's published
  surface), `routing.task-type-research` (ADR-0006),
  `execution.resource-budgets` (ADR-0010, a baseline budget),
  `work.implementation-review`, `work.verification-loop-before-push`.
- **Prior programme:** decisions `docs/programmes/one-verdict-per-tree`
  decided that relais verifies through amont gates (t01, t03) and skips a
  proven base (t04). This map finds why that is not in effect, and finishes
  it.
- **Neighbouring programme:** relais `docs/programmes/homelab-offload` decides
  where a heavy check runs. Its t14 verdict key must still be the one amont
  writes.
- **In flight on 2026-11-10 (board):** `fix/budget-cut-baseline-and-gate-test`
  (baseline budget) and `feat/local-model-rung` (Qwen first) touch the same
  stages; ask before closing either.
- **Cost:** research runs are read-only, through relais `inspect` or an
  Explore agent. A relais research worker has no shell, so ledger queries
  run in the session.

## Decisions so far

- [What reads relais's baseline, verification and review results today](t01-verdict-consumers.md): stamp-proven base already built; the reviewer is ~2,400 lines plus the inspect report review (inspect's only verification) and the forced review on edited tests; learner unaffected
- [How much check and review time relais spends that a later gate repeats](t02-duplicate-check-cost.md): since 10-10, 21 h of baseline, 23 h of verification, $441 of review (38% of $1,151); every check repeats at push, the review at F4b; exact share unjoined
- [What stops amont's push gate from reusing a relais candidate's verdict](t03-attempt-ends-on-amont.md): only the profiles: amont's runs `make check`, so nothing is stamped; relais's side and the stamp keying already work; split the profile per gate
- [Which repositories' relais profiles can split into amont gates](t09-profiles-per-gate.md): the six Rust repositories split cleanly (fmt, clippy, cargo-test) with a few plain leftovers; happier cannot; JS/Go/Python matches unverified

## Not yet specified

- What the plugin pane and the `decide` flow show once `review_findings` and
  the reviewer's `needs_review` are gone.
- Whether Anthropic's advisor tool replaces the repair and escalation ladder
  for Claude workers, and what stays for local Qwen workers.

## Out of scope

- [What the router learns from once the verdict comes from amont](t04-router-learning-label.md): relais keeps its verdict, from either adapter, so the label does not change
- [What relais's documented interface to amont must specify](t06-amont-interface.md): without amont, relais runs its checks itself; there is no interface for a user to implement
