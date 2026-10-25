---
canonical: decisions:docs/plans/2026-10-24-code-rules-reach-the-reviewer.md
phases: [4]
status: active
branch: feat/candidates-in-review
---
# Code rules reach the reviewer (pointer)

Canonical plan: [decisions/docs/plans/2026-10-24-code-rules-reach-the-reviewer.md](https://github.com/fredericrous/decisions/blob/main/docs/plans/2026-10-24-code-rules-reach-the-reviewer.md) (ADR-0044).

This repository carries relais's half of Phase 4 (behaviour 3):
- the reviewer prompt lists adr-lint's candidates for the candidate commit (`adr-lint candidates --base <base> --head <candidate> --json`), as a new `GuidanceSource`;
- a missing or failing adr-lint is a receipt gap, `candidates missing (…)`, and never a silent drop;
- the receipt counts the dispositions (violation, fine, undisposed);
- each ruling is written as an `adr-lint.ruling/1` record with `tool: relais`.

## Verification

Input → expected → actual:
- `make lint`, `make test` → pass → pass (2240 tests passed, 0 failed); the commit gate → 30 checks pass → pass, after the fake adr-lint was primed (`test_support::prime_script`). Unprimed, a freshly written script lost its first exec to the 5 s guidance timeout under load once.
- The captured adr-lint 0.3.0 document, a wrong `format` token, an uppercase id and a 63-hex id → parsed / Unparseable gap ×3 → as expected (context/candidates.rs tests).
- A fake adr-lint that exits 3, and a missing binary → `candidates missing (` in the receipt note, the reviewer still dispatched, `CandidatesRecord::Unavailable` → as expected.
- A two-part review → each candidate listed in exactly one part → as expected. Three listed candidates with 2 ruled → `undisposed: 1` → as expected. `rulings.jsonl` read back → the field shapes of `adr-lint.ruling/1` → as expected.
- The real adr-lint 0.4.0, run from the primary checkout (on `main`, a different working tree) with `--base a60e1701 --head be5b8d99` → the 4 ids amont-agent's brief listed with the commit checked out → the same 4 ids, at the same paths and lines. So the repository directory sees the candidate commit, and text and lines come from `--head`, in every workspace mode.

## Implementation review

- approve-with-changes (1 blocking, 1 medium, 1 low), then approve on the Delta; 70k + 80k tokens, 117 s + 27 s.
- Fixed:
  - the `stopped_run(.., bool)` flag argument is split in two. adr-lint listed this as a candidate, and the reviewer ruled it a violation: the first real catch;
  - a ruling's `repo` is the main checkout's name;
  - SPEC §7 says what `repo` and `tree` hold.
- The reviewer ruled the other 3 candidates `fine`; their ids stayed the same across both rounds.
- After that approval, relais#307's Windows test job failed to compile: the fake adr-lint is a shell script, written with Unix-only APIs. The fix gates only what needs the script with `#[cfg(unix)]`: the exit-3 test and the argv test. The missing-binary test and the spawning resolver run on every platform; the reviewer asked for that split. Both re-bindings: approve-with-changes, then approve; 84k + 90k tokens.
- The relais runs: `run-65ea960a85370-d257` was approved after its parts review, then applied by hand (an approval does not apply the candidate), with its one citation fix made by hand.
