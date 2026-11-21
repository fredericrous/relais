---
canonical: decisions:docs/plans/2026-11-21-approved-programmes-run-without-person.md
phases: [2]
status: done
---
Part of [Approved programmes run without the person](https://github.com/fredericrous/decisions/blob/main/docs/plans/2026-11-21-approved-programmes-run-without-person.md).

## Code review

- relais: backend, lang:rust (unix, tui left out: no command-line output or flag changes, only skill text and tests)

## Verification

- `cargo test -p relais --lib install::` → the new pin passes → passed.
- `at most 5` changed to `at most five` in SKILL.md → the pin fails and names the phrase → failed with ``the skill lost `at most 5 per drive` ``; passed once restored.
- `a_diamond_frees_both_branches_then_their_join` → B and D ready after A, C only after both → passed (with `check` exiting 0 on the first map).

## Implementation review

- implementation-review: approve-with-changes, then delta approve; fixed: plan record, ADR-0079 exemption logged, research exception kept, fog disposition; 79k + 86k.
- backend: approve-with-changes, then approve; fixed: claim stderr words for exits 1 and 3, no handoff mid-run; deliberate: `stayed held for` pin filed as a follow-up; 67k + 72k.
- lang:rust: approve-with-changes, then approve; fixed: closure parameter renamed; 48k + 54k.
